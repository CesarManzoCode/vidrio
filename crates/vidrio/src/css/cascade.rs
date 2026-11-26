//! Rule collection and the cascade (css-cascade-5 and -6): the rules of
//! the user agent sheet and of the author sheets of each tree (the
//! document and every shadow root) indexed by the subject compound of
//! their selectors (id, class, type, the rest), a counting bloom filter
//! of the ancestors that skips rules whose ancestor compounds cannot
//! match, nesting desugared to `:is()`, `@media`, `@supports`, `@import`,
//! cascade layers (with their order, anonymous layers and `@import
//! layer`), `@scope` with its proximity, `:host` and `::slotted()` rules
//! of shadow trees, and the order of the cascade: origin and importance,
//! shadow context, element-attached declarations, layers, specificity,
//! scope proximity and order of appearance. `revert` and `revert-layer`
//! roll back along that order ([`Cascaded::winner`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use super::declaration::{Declaration, DeclarationBlock, serialize_tokens};
use super::hints;
use super::media::Environment;
use super::om::{self, RuleKind, RuleRef, SheetRef};
use super::properties;
use super::selectors::{
    Combinator, DefaultStates, MatchContext, NamespaceMap, PseudoElement, Selector, SelectorList,
    Simple, Specificity, matches_originating, matches_selector, matches_selector_list,
    matches_slotted, parse_selector_list_with,
};
use super::sheets;
use super::tokenizer::{Token, tokenize};
use super::values::custom::{Registration, Registry};
use crate::dom::{DocRef, Document, Namespace, NodeId};

/// A cascade origin (vidrio has no user style sheets).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    UserAgent,
    Author,
}

/// The pseudo-elements that get a computed style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pseudo {
    Before,
    After,
    Marker,
    FirstLine,
    FirstLetter,
    Placeholder,
}

impl Pseudo {
    pub const ALL: [Pseudo; 6] = [
        Pseudo::Before,
        Pseudo::After,
        Pseudo::Marker,
        Pseudo::FirstLine,
        Pseudo::FirstLetter,
        Pseudo::Placeholder,
    ];

    fn of(pe: &PseudoElement) -> Option<Pseudo> {
        Some(match pe {
            PseudoElement::Before => Pseudo::Before,
            PseudoElement::After => Pseudo::After,
            PseudoElement::Marker => Pseudo::Marker,
            PseudoElement::FirstLine => Pseudo::FirstLine,
            PseudoElement::FirstLetter => Pseudo::FirstLetter,
            PseudoElement::Placeholder => Pseudo::Placeholder,
            _ => return None,
        })
    }

    /// The pseudo-element of a `getComputedStyle` argument: `::name`, or
    /// `:name` for the four legacy ones (ASCII case-insensitive).
    pub fn parse(text: &str) -> Option<Pseudo> {
        let lower = text.to_ascii_lowercase();
        let (name, legacy) = match lower.strip_prefix("::") {
            Some(n) => (n, false),
            None => (lower.strip_prefix(':')?, true),
        };
        let p = match name {
            "before" => Pseudo::Before,
            "after" => Pseudo::After,
            "first-line" => Pseudo::FirstLine,
            "first-letter" => Pseudo::FirstLetter,
            "marker" if !legacy => Pseudo::Marker,
            "placeholder" if !legacy => Pseudo::Placeholder,
            _ => return None,
        };
        Some(p)
    }
}

// ----- the bloom filter -------------------------------------------------------------------

const BLOOM_BITS: usize = 4096;

/// FNV-1a over a kind byte and the ASCII-lowercased name.
fn hash(kind: u8, name: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in std::iter::once(kind).chain(name.bytes().map(|b| b.to_ascii_lowercase())) {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn bloom_indices(h: u32) -> [usize; 2] {
    [
        (h as usize) % BLOOM_BITS,
        ((h >> 12) as usize ^ (h >> 24) as usize * 7) % BLOOM_BITS,
    ]
}

/// A counting bloom filter of the ids, classes and local names of the
/// ancestors of the element being styled.
pub struct Bloom {
    counters: Vec<u8>,
}

impl Default for Bloom {
    fn default() -> Bloom {
        Bloom {
            counters: vec![0; BLOOM_BITS],
        }
    }
}

impl Bloom {
    fn add(&mut self, h: u32) {
        for i in bloom_indices(h) {
            self.counters[i] = self.counters[i].saturating_add(1);
        }
    }

    fn remove(&mut self, h: u32) {
        for i in bloom_indices(h) {
            // A saturated counter stays: it may stand for more entries.
            if self.counters[i] != u8::MAX {
                self.counters[i] = self.counters[i].saturating_sub(1);
            }
        }
    }

    fn may_contain(&self, h: u32) -> bool {
        bloom_indices(h).iter().all(|&i| self.counters[i] > 0)
    }

    /// Adds an element as an ancestor of what is styled next.
    pub fn push(&mut self, doc: &Document, element: NodeId) {
        for h in element_hashes(doc, element) {
            self.add(h);
        }
    }

    /// Removes an element pushed before.
    pub fn pop(&mut self, doc: &Document, element: NodeId) {
        for h in element_hashes(doc, element) {
            self.remove(h);
        }
    }
}

fn element_hashes(doc: &Document, element: NodeId) -> Vec<u32> {
    let Some(e) = doc.element(element) else {
        return Vec::new();
    };
    let mut out = vec![hash(b't', e.name.local.as_str())];
    if let Some(id) = doc.get_attribute(element, "id") {
        if !id.is_empty() {
            out.push(hash(b'#', id));
        }
    }
    if let Some(class) = doc.get_attribute(element, "class") {
        for c in class.split_ascii_whitespace() {
            out.push(hash(b'.', c));
        }
    }
    out
}

/// The hashes of the ids, classes and types an ancestor of the subject
/// must have: the compounds joined to their right by a child or
/// descendant combinator are ancestors of the subject.
fn ancestor_hashes(sel: &Selector) -> Vec<u32> {
    let mut out = Vec::new();
    for (i, comb) in sel.combinators.iter().enumerate() {
        if !matches!(comb, Combinator::Child | Combinator::Descendant) {
            continue;
        }
        for simple in &sel.compounds[i].0 {
            match simple {
                Simple::Id(id) => out.push(hash(b'#', id)),
                Simple::Class(c) => out.push(hash(b'.', c)),
                Simple::Type { local, .. } => out.push(hash(b't', local)),
                _ => {}
            }
        }
    }
    out
}

// ----- cascade layers --------------------------------------------------------------------

/// The layers of one origin in one tree: a tree of names whose order is
/// the order of first declaration; the declarations directly in a layer
/// come after its sublayers, and unlayered ones (the root) after all.
#[derive(Debug)]
struct Layers {
    /// Name and children of each layer; 0 is the root.
    nodes: Vec<(String, Vec<usize>)>,
}

impl Default for Layers {
    fn default() -> Layers {
        Layers {
            nodes: vec![(String::new(), Vec::new())],
        }
    }
}

impl Layers {
    fn child(&mut self, parent: usize, name: &str) -> usize {
        if let Some(&c) = self.nodes[parent]
            .1
            .iter()
            .find(|&&c| !self.nodes[c].0.is_empty() && self.nodes[c].0 == name)
        {
            return c;
        }
        self.nodes.push((name.to_string(), Vec::new()));
        let id = self.nodes.len() - 1;
        self.nodes[parent].1.push(id);
        id
    }

    /// The layer `a.b.c` under `parent`, created as needed.
    fn named(&mut self, parent: usize, dotted: &str) -> usize {
        dotted
            .split('.')
            .fold(parent, |p, part| self.child(p, part.trim()))
    }

    /// A new anonymous layer under `parent`.
    fn anonymous(&mut self, parent: usize) -> usize {
        self.nodes.push((String::new(), Vec::new()));
        let id = self.nodes.len() - 1;
        self.nodes[parent].1.push(id);
        id
    }

    /// The rank of each layer (higher wins for normal declarations).
    fn ranks(&self) -> Vec<i64> {
        let mut ranks = vec![0; self.nodes.len()];
        let mut next = 0;
        // Post-order without recursion.
        let mut stack = vec![(0usize, 0usize)];
        while let Some((node, child)) = stack.pop() {
            if let Some(&c) = self.nodes[node].1.get(child) {
                stack.push((node, child + 1));
                stack.push((c, 0));
            } else {
                ranks[node] = next;
                next += 1;
            }
        }
        ranks
    }
}

// ----- @scope ----------------------------------------------------------------------------

/// The start of a scope.
#[derive(Debug)]
enum ScopeStart {
    Selector(SelectorList),
    /// No `<scope-start>`: the parent element of the sheet's owner node,
    /// or the top element of the tree.
    Implicit(Option<NodeId>),
}

/// An `@scope` rule in effect.
#[derive(Debug)]
pub struct Scope {
    start: ScopeStart,
    end: Option<SelectorList>,
    parent: Option<Rc<Scope>>,
}

/// The tree root of `node` (the document or a shadow root).
fn tree_root(doc: &Document, node: NodeId) -> NodeId {
    let mut n = node;
    while let Some(p) = doc.parent(n) {
        n = p;
    }
    n
}

/// The parent element of `node` in its tree.
fn parent_element(doc: &Document, node: NodeId) -> Option<NodeId> {
    doc.parent(node).filter(|&p| doc.is_element(p))
}

/// The scoping roots of `scope` that `element` is in scope of, nearest
/// first, with the generational hops from each root to `element`.
fn scope_roots(
    doc: &Document,
    scope: &Scope,
    element: NodeId,
    ctx: &MatchContext,
) -> Vec<(NodeId, u32)> {
    let mut out = Vec::new();
    let mut path = Vec::new();
    let mut current = Some(element);
    let mut hops = 0;
    while let Some(a) = current {
        path.push(a);
        let outer: Option<NodeId> = match &scope.parent {
            Some(p) => match scope_roots(doc, p, a, ctx).first() {
                Some(&(r, _)) => Some(r),
                None => {
                    current = parent_element(doc, a);
                    hops += 1;
                    continue;
                }
            },
            None => None,
        };
        let is_root = match &scope.start {
            ScopeStart::Selector(list) => {
                matches_selector_list(doc, list, a, &ctx.with_scope(outer))
            }
            ScopeStart::Implicit(Some(p)) => a == *p,
            ScopeStart::Implicit(None) => parent_element(doc, a).is_none(),
        };
        if is_root {
            let limited = scope.end.as_ref().is_some_and(|end| {
                path[..path.len() - 1]
                    .iter()
                    .any(|&l| matches_selector_list(doc, end, l, &ctx.with_scope(Some(a))))
            });
            if !limited {
                out.push((a, hops));
            }
        }
        current = parent_element(doc, a);
        hops += 1;
    }
    out
}

// ----- rule sets -------------------------------------------------------------------------

/// One selector of a style rule with what the cascade needs.
#[derive(Debug)]
struct Entry {
    selector: Selector,
    block: Rc<DeclarationBlock>,
    /// The order of appearance of the rule (shared by its selectors).
    order: u32,
    layer: usize,
    scope: Option<Rc<Scope>>,
    hashes: Vec<u32>,
}

/// The rules of one origin in one tree.
#[derive(Debug, Default)]
pub struct RuleSet {
    entries: Vec<Entry>,
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_type: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
    pseudo: Vec<(Pseudo, usize)>,
    slotted: Vec<usize>,
    layers: Layers,
    ranks: Vec<i64>,
    /// The `@property` rules, in order.
    properties: Vec<Registration>,
}

impl RuleSet {
    fn push(&mut self, entry: Entry) {
        let i = self.entries.len();
        match &entry.selector.pseudo_element {
            Some(PseudoElement::Slotted(_)) => self.slotted.push(i),
            Some(pe) => match Pseudo::of(pe) {
                Some(p) => self.pseudo.push((p, i)),
                None => return,
            },
            None => {
                let (mut id, mut class, mut ty) = (None, None, None);
                for s in &entry.selector.subject().0 {
                    match s {
                        Simple::Id(v) if id.is_none() => id = Some(v.to_ascii_lowercase()),
                        Simple::Class(v) if class.is_none() => class = Some(v.to_ascii_lowercase()),
                        Simple::Type { local, .. } if ty.is_none() => {
                            ty = Some(local.to_ascii_lowercase())
                        }
                        _ => {}
                    }
                }
                if let Some(id) = id {
                    self.by_id.entry(id).or_default().push(i);
                } else if let Some(c) = class {
                    self.by_class.entry(c).or_default().push(i);
                } else if let Some(t) = ty {
                    self.by_type.entry(t).or_default().push(i);
                } else {
                    self.universal.push(i);
                }
            }
        }
        self.entries.push(entry);
    }

    /// The highest layer rank (unlayered declarations).
    fn unlayered(&self) -> i64 {
        self.ranks.first().copied().unwrap_or(0)
    }

    /// The entries that may match an element with these hashes.
    fn candidates(&self, doc: &Document, element: NodeId) -> Vec<usize> {
        let mut out = self.universal.clone();
        let Some(e) = doc.element(element) else {
            return out;
        };
        if let Some(v) = self
            .by_type
            .get(&e.name.local.as_str().to_ascii_lowercase())
        {
            out.extend(v);
        }
        if let Some(id) = doc.get_attribute(element, "id") {
            if let Some(v) = self.by_id.get(&id.to_ascii_lowercase()) {
                out.extend(v);
            }
        }
        if let Some(class) = doc.get_attribute(element, "class") {
            let mut seen: Vec<String> = Vec::new();
            for c in class.split_ascii_whitespace() {
                let c = c.to_ascii_lowercase();
                if seen.contains(&c) {
                    continue;
                }
                if let Some(v) = self.by_class.get(&c) {
                    out.extend(v);
                }
                seen.push(c);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Where a rule sits while the sheets are walked.
#[derive(Clone)]
struct RuleCtx {
    layer: usize,
    /// The effective selector of the enclosing style rule.
    parent: Option<String>,
    scope: Option<Rc<Scope>>,
    /// Directly in an `@scope` body: selectors are scope-relative.
    scope_relative: bool,
    owner: Option<NodeId>,
}

/// Walks sheets into a [`RuleSet`].
struct Builder<'a> {
    env: &'a Environment,
    doc: &'a Document,
    set: RuleSet,
    order: &'a mut u32,
}

fn split_commas(tokens: &[Token]) -> Vec<Vec<Token>> {
    let mut parts = vec![Vec::new()];
    let mut depth = 0usize;
    for t in tokens {
        match t {
            Token::Function(_) | Token::LeftParen | Token::LeftBracket | Token::LeftBrace => {
                depth += 1
            }
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth = depth.saturating_sub(1)
            }
            Token::Comma if depth == 0 => {
                parts.push(Vec::new());
                continue;
            }
            _ => {}
        }
        parts.last_mut().expect("a part").push(t.clone());
    }
    parts
        .into_iter()
        .map(|p| {
            let mut p: &[Token] = &p;
            while p.first() == Some(&Token::Whitespace) {
                p = &p[1..];
            }
            while p.last() == Some(&Token::Whitespace) {
                p = &p[..p.len() - 1];
            }
            p.to_vec()
        })
        .collect()
}

fn has_scope_pseudo(part: &[Token]) -> bool {
    part.windows(2).any(|w| {
        w[0] == Token::Colon && matches!(&w[1], Token::Ident(i) if i.eq_ignore_ascii_case("scope"))
    })
}

/// Replaces every nesting selector `&` of `part` with `replacement`.
fn replace_nesting(part: &[Token], replacement: &str) -> Vec<Token> {
    let rep = tokenize(replacement);
    let mut out = Vec::with_capacity(part.len());
    for t in part {
        if *t == Token::Delim('&') {
            out.extend(rep.iter().cloned());
        } else {
            out.push(t.clone());
        }
    }
    out
}

/// The selector a style rule matches with: nesting desugared to `:is()`
/// of the parent's selector (with an implied descendant combinator when
/// a part has no `&`), and scope-relative selectors prefixed with
/// `:where(:scope)` unless they name `:scope` or `&`.
fn desugar(selector: &str, ctx: &RuleCtx) -> String {
    let tokens = tokenize(selector);
    let parts: Vec<String> = split_commas(&tokens)
        .into_iter()
        .map(|part| {
            let has_amp = part.contains(&Token::Delim('&'));
            let out = if let Some(parent) = &ctx.parent {
                let rep = format!(":is({parent})");
                if has_amp {
                    replace_nesting(&part, &rep)
                } else {
                    let mut v = tokenize(&rep);
                    v.push(Token::Whitespace);
                    v.extend(part);
                    v
                }
            } else if ctx.scope_relative {
                if has_amp {
                    replace_nesting(&part, ":scope")
                } else if has_scope_pseudo(&part) {
                    part
                } else {
                    let mut v = tokenize(":where(:scope)");
                    v.push(Token::Whitespace);
                    v.extend(part);
                    v
                }
            } else {
                replace_nesting(&part, ":scope")
            };
            serialize_tokens(&out)
        })
        .collect();
    parts.join(", ")
}

fn namespaces(rules: &[RuleRef]) -> NamespaceMap {
    let mut map = NamespaceMap::default();
    for r in rules {
        if let RuleKind::Namespace { prefix, uri } = &r.borrow().kind {
            let ns = Namespace::from_url(uri);
            if prefix.is_empty() {
                map.default = Some(ns);
            } else {
                map.prefixes.retain(|(p, _)| p != prefix);
                map.prefixes.push((prefix.clone(), ns));
            }
        }
    }
    map
}

impl Builder<'_> {
    fn sheet(&mut self, sheet: &SheetRef, ctx: &RuleCtx, depth: u32) {
        if depth > 16 {
            return;
        }
        let rules = sheet.borrow().rules.clone();
        let ns = namespaces(&rules);
        self.rules(&rules, &ns, ctx, depth);
    }

    fn next_order(&mut self) -> u32 {
        *self.order += 1;
        *self.order
    }

    fn style(&mut self, text: &str, block: &DeclarationBlock, ns: &NamespaceMap, ctx: &RuleCtx) {
        let Ok(list) = parse_selector_list_with(text, ns) else {
            return;
        };
        let block = Rc::new(block.clone());
        let order = self.next_order();
        for selector in list.0 {
            let hashes = ancestor_hashes(&selector);
            self.set.push(Entry {
                selector,
                block: block.clone(),
                order,
                layer: ctx.layer,
                scope: ctx.scope.clone(),
                hashes,
            });
        }
    }

    fn layer_of(&mut self, parent: usize, name: &str) -> usize {
        if name.is_empty() {
            self.set.layers.anonymous(parent)
        } else {
            self.set.layers.named(parent, name)
        }
    }

    fn rules(&mut self, rules: &[RuleRef], ns: &NamespaceMap, ctx: &RuleCtx, depth: u32) {
        for r in rules {
            let rule = r.borrow();
            match &rule.kind {
                RuleKind::Style {
                    selector,
                    style,
                    rules: children,
                } => {
                    let text = desugar(selector, ctx);
                    if !style.is_empty() {
                        self.style(&text, style, ns, ctx);
                    }
                    if !children.is_empty() {
                        let inner = RuleCtx {
                            parent: Some(text),
                            scope_relative: false,
                            ..ctx.clone()
                        };
                        self.rules(children, ns, &inner, depth);
                    }
                }
                RuleKind::NestedDeclarations { style } => {
                    let text = match &ctx.parent {
                        Some(p) => p.clone(),
                        None if ctx.scope_relative => ":where(:scope)".to_string(),
                        None => continue,
                    };
                    self.style(&text, style, ns, ctx);
                }
                RuleKind::Media { media, rules } => {
                    if media.matches(self.env) {
                        self.rules(rules, ns, ctx, depth);
                    }
                }
                RuleKind::Supports { matches, rules, .. } => {
                    if *matches {
                        self.rules(rules, ns, ctx, depth);
                    }
                }
                RuleKind::LayerBlock { name, rules } => {
                    let layer = self.layer_of(ctx.layer, name);
                    let inner = RuleCtx {
                        layer,
                        ..ctx.clone()
                    };
                    self.rules(rules, ns, &inner, depth);
                }
                RuleKind::LayerStatement { names } => {
                    for n in names {
                        self.set.layers.named(ctx.layer, n);
                    }
                }
                RuleKind::Import {
                    media,
                    layer,
                    supports,
                    sheet: Some(sheet),
                    ..
                } => {
                    if !media.matches(self.env) || sheet.borrow().disabled {
                        continue;
                    }
                    if supports
                        .as_deref()
                        .is_some_and(|s| !om::css_supports_condition(s))
                    {
                        continue;
                    }
                    let layer = match layer {
                        Some(name) => self.layer_of(ctx.layer, name),
                        None => ctx.layer,
                    };
                    let inner = RuleCtx {
                        layer,
                        ..ctx.clone()
                    };
                    self.sheet(sheet, &inner, depth + 1);
                }
                RuleKind::Scope { start, end, rules } => {
                    let amp = |s: &str| match &ctx.parent {
                        Some(p) => {
                            serialize_tokens(&replace_nesting(&tokenize(s), &format!(":is({p})")))
                        }
                        None => serialize_tokens(&replace_nesting(&tokenize(s), ":scope")),
                    };
                    let start = match (start, &ctx.parent) {
                        (Some(s), _) => match parse_selector_list_with(&amp(s), ns) {
                            Ok(l) => ScopeStart::Selector(l),
                            Err(_) => continue,
                        },
                        (None, Some(p)) => match parse_selector_list_with(p, ns) {
                            Ok(l) => ScopeStart::Selector(l),
                            Err(_) => continue,
                        },
                        (None, None) => ScopeStart::Implicit(
                            ctx.owner.and_then(|o| parent_element(self.doc, o)),
                        ),
                    };
                    let end = match end {
                        Some(e) => {
                            let text = serialize_tokens(&replace_nesting(&tokenize(e), ":scope"));
                            match parse_selector_list_with(&text, ns) {
                                Ok(l) => Some(l),
                                Err(_) => continue,
                            }
                        }
                        None => None,
                    };
                    let scope = Rc::new(Scope {
                        start,
                        end,
                        parent: ctx.scope.clone(),
                    });
                    let inner = RuleCtx {
                        parent: None,
                        scope: Some(scope),
                        scope_relative: true,
                        ..ctx.clone()
                    };
                    self.rules(rules, ns, &inner, depth);
                }
                RuleKind::Property { name, style } => {
                    let Some(syntax) = om::property_syntax(style) else {
                        continue;
                    };
                    let Some(inherits) = om::property_inherits(style) else {
                        continue;
                    };
                    let initial = style.property_value("initial-value");
                    let initial = (!initial.is_empty()).then_some(initial);
                    if let Ok(r) = Registration::new(name, &syntax, inherits, initial.as_deref()) {
                        self.set.properties.push(r);
                    }
                }
                _ => {}
            }
        }
    }
}

thread_local! {
    /// The user agent sheet, parsed once.
    static UA_SHEET: SheetRef = om::parse_sheet(include_str!("ua.css"));
    /// The registrations of `CSS.registerProperty`, per document.
    static REGISTERED: RefCell<Vec<(Weak<RefCell<Document>>, Registry)>> =
        const { RefCell::new(Vec::new()) };
}

/// The user agent style sheet (`ua.css`).
pub fn ua_sheet() -> SheetRef {
    UA_SHEET.with(Rc::clone)
}

/// Records a `CSS.registerProperty` registration for `doc`; false if the
/// name is registered already.
pub fn register_property(doc: &DocRef, r: Registration) -> bool {
    let ok = REGISTERED.with(|reg| {
        let mut reg = reg.borrow_mut();
        reg.retain(|(d, _)| d.strong_count() > 0);
        let i = match reg.iter().position(|(d, _)| d.as_ptr() == Rc::as_ptr(doc)) {
            Some(i) => i,
            None => {
                reg.push((Rc::downgrade(doc), Registry::default()));
                reg.len() - 1
            }
        };
        reg[i].1.register(r).is_ok()
    });
    if ok {
        om::sheets_changed();
    }
    ok
}

fn registered(doc: &DocRef) -> Registry {
    REGISTERED.with(|reg| {
        reg.borrow()
            .iter()
            .find(|(d, _)| d.as_ptr() == Rc::as_ptr(doc))
            .map(|(_, r)| r.clone())
            .unwrap_or_default()
    })
}

/// The sheets that apply to each tree of `doc` (the document and its
/// shadow roots) with the shadow depth of the tree and the owner node
/// of each sheet. Asking for them brings `style` sheets up to date.
pub struct TreeSheets {
    pub trees: Vec<(NodeId, u32, Vec<(Option<NodeId>, SheetRef)>)>,
}

/// The shadow roots of `doc` with their depth (the document is 0).
fn shadow_roots(doc: &Document) -> Vec<(NodeId, u32)> {
    let mut out = vec![(NodeId::DOCUMENT, 0)];
    let mut i = 0;
    while i < out.len() {
        let (root, depth) = out[i];
        for n in doc.inclusive_descendants(root) {
            if doc.is_element(n) {
                if let Some(sr) = doc.shadow_root_of(n) {
                    out.push((sr, depth + 1));
                }
            }
        }
        i += 1;
    }
    out
}

/// Collects the applicable sheets of every tree of `doc`.
pub fn tree_sheets(doc: &DocRef, env: &Environment) -> TreeSheets {
    let roots = shadow_roots(&doc.borrow());
    let mut trees = Vec::new();
    for (root, depth) in roots {
        let mut list: Vec<(Option<NodeId>, SheetRef)> = sheets::style_sheets(doc, root)
            .into_iter()
            .map(|(n, s)| (Some(n), s))
            .collect();
        list.extend(sheets::adopted(doc, root).into_iter().map(|s| (None, s)));
        list.retain(|(_, s)| {
            let s = s.borrow();
            !s.disabled && !s.alternate && s.media.matches(env)
        });
        trees.push((root, depth, list));
    }
    TreeSheets { trees }
}

/// The rules of a document: the user agent sheet and the author sheets
/// of each tree, and the registered custom properties.
pub struct Stylist {
    ua: RuleSet,
    trees: HashMap<NodeId, (u32, RuleSet)>,
    pub registry: Registry,
}

impl Stylist {
    /// Builds the rule sets from the sheets [`tree_sheets`] gave.
    pub fn build(doc: &DocRef, sheets: &TreeSheets, env: &Environment) -> Stylist {
        let d = doc.borrow();
        let mut order = 0;
        let base = RuleCtx {
            layer: 0,
            parent: None,
            scope: None,
            scope_relative: false,
            owner: None,
        };
        let mut ua = Builder {
            env,
            doc: &d,
            set: RuleSet::default(),
            order: &mut order,
        };
        ua.sheet(&ua_sheet(), &base, 0);
        let mut ua = ua.set;
        ua.ranks = ua.layers.ranks();
        let mut trees = HashMap::new();
        let mut properties = Vec::new();
        for (root, depth, list) in &sheets.trees {
            let mut b = Builder {
                env,
                doc: &d,
                set: RuleSet::default(),
                order: &mut order,
            };
            for (owner, sheet) in list {
                let ctx = RuleCtx {
                    owner: *owner,
                    ..base.clone()
                };
                b.sheet(sheet, &ctx, 0);
            }
            let mut set = b.set;
            set.ranks = set.layers.ranks();
            if *root == NodeId::DOCUMENT {
                properties = std::mem::take(&mut set.properties);
            }
            trees.insert(*root, (*depth, set));
        }
        drop(d);
        let mut registry = registered(doc);
        // The last `@property` of a name wins; `registerProperty` wins
        // over every `@property`.
        for r in properties.into_iter().rev() {
            let _ = registry.register(r);
        }
        Stylist {
            ua,
            trees,
            registry,
        }
    }
}

// ----- the cascade -----------------------------------------------------------------------

/// The precedence of a declaration: compared lexicographically, the
/// greater wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    /// UA normal 0, author normal 1, author important 2, UA important 3.
    rank: u8,
    /// Normal: outer trees win; important: inner trees win.
    context: i64,
    /// The `style` attribute wins over rules.
    attached: u8,
    /// Normal: later layers win; important: earlier layers win.
    layer: i64,
    specificity: Specificity,
    /// Nearer scoping roots win; unscoped declarations lose.
    proximity: u32,
    order: u32,
    /// The position of the declaration in its block.
    position: u32,
}

impl Key {
    /// The part of the key `revert-layer` rolls back past.
    fn group(&self) -> (u8, i64, u8, i64) {
        (self.rank, self.context, self.attached, self.layer)
    }
}

/// A matched declaration block with one importance.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub block: Rc<DeclarationBlock>,
    pub origin: Origin,
    pub important: bool,
    key: Key,
}

#[allow(clippy::too_many_arguments)]
fn candidates_of(
    out: &mut Vec<Candidate>,
    block: &Rc<DeclarationBlock>,
    origin: Origin,
    depth: u32,
    attached: bool,
    layer_rank: i64,
    specificity: Specificity,
    hops: Option<u32>,
    order: u32,
) {
    for important in [false, true] {
        if !block.declarations.iter().any(|d| d.important == important) {
            continue;
        }
        let rank = match (origin, important) {
            (Origin::UserAgent, false) => 0,
            (Origin::Author, false) => 1,
            (Origin::Author, true) => 2,
            (Origin::UserAgent, true) => 3,
        };
        let depth = i64::from(depth);
        out.push(Candidate {
            block: block.clone(),
            origin,
            important,
            key: Key {
                rank,
                context: if important { depth } else { -depth },
                attached: u8::from(attached),
                layer: if important { -layer_rank } else { layer_rank },
                specificity,
                proximity: hops.map_or(0, |h| u32::MAX - h),
                order,
                position: 0,
            },
        });
    }
}

/// The shadow host of the tree `root`, if it is a shadow root.
fn host_of(doc: &Document, root: NodeId) -> Option<NodeId> {
    doc.shadow_root_host(root)
}

/// Matches the entries `ids` of `set` against `element`.
#[allow(clippy::too_many_arguments)]
fn match_entries(
    out: &mut Vec<Candidate>,
    doc: &Document,
    set: &RuleSet,
    ids: &[usize],
    element: NodeId,
    origin: Origin,
    depth: u32,
    ctx: &MatchContext,
    bloom: Option<&Bloom>,
    how: fn(&Document, &Selector, NodeId, &MatchContext) -> bool,
) {
    // The best match of each rule: (order, specificity, hops, entry).
    let mut best: Vec<(u32, Specificity, Option<u32>, usize)> = Vec::new();
    for &i in ids {
        let e = &set.entries[i];
        if let Some(b) = bloom {
            if !e.hashes.iter().all(|&h| b.may_contain(h)) {
                continue;
            }
        }
        let found = match &e.scope {
            None => how(doc, &e.selector, element, ctx).then_some(None),
            Some(scope) => scope_roots(doc, scope, element, ctx)
                .into_iter()
                .find(|&(root, _)| how(doc, &e.selector, element, &ctx.with_scope(Some(root))))
                .map(|(_, h)| Some(h)),
        };
        let Some(hops) = found else {
            continue;
        };
        let spec = e.selector.specificity;
        match best.iter_mut().find(|b| b.0 == e.order) {
            Some(b) => {
                if spec > b.1 {
                    b.1 = spec;
                }
                if let (Some(h), Some(bh)) = (hops, b.2) {
                    b.2 = Some(h.min(bh));
                }
            }
            None => best.push((e.order, spec, hops, i)),
        }
    }
    for (order, spec, hops, i) in best {
        let e = &set.entries[i];
        let rank = set.ranks.get(e.layer).copied().unwrap_or(0);
        candidates_of(out, &e.block, origin, depth, false, rank, spec, hops, order);
    }
}

/// The declaration blocks that apply to `element` (or to its
/// pseudo-element `pseudo`), with their precedence.
pub fn collect(
    stylist: &Stylist,
    doc: &Document,
    element: NodeId,
    pseudo: Option<Pseudo>,
    bloom: Option<&Bloom>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    let states = DefaultStates;
    let plain = MatchContext::new(&states);
    let root = tree_root(doc, element);
    let (depth, tree) = match stylist.trees.get(&root) {
        Some((d, s)) => (*d, Some(s)),
        None => (0, None),
    };
    let tree_ctx = plain.with_shadow_host(host_of(doc, root));
    let pseudo_ids = |set: &RuleSet| -> Vec<usize> {
        set.pseudo
            .iter()
            .filter(|(p, _)| Some(*p) == pseudo)
            .map(|(_, i)| *i)
            .collect()
    };
    let (how, ua_ids): (
        fn(&Document, &Selector, NodeId, &MatchContext) -> bool,
        Vec<usize>,
    ) = match pseudo {
        None => (matches_selector, stylist.ua.candidates(doc, element)),
        Some(_) => (matches_originating, pseudo_ids(&stylist.ua)),
    };
    match_entries(
        &mut out,
        doc,
        &stylist.ua,
        &ua_ids,
        element,
        Origin::UserAgent,
        0,
        &plain,
        bloom,
        how,
    );
    if pseudo.is_none() {
        // Presentational hints: author declarations below every layer.
        let hints = hints::presentational_hints(doc, element);
        if !hints.is_empty() {
            candidates_of(
                &mut out,
                &Rc::new(hints),
                Origin::Author,
                depth,
                false,
                -1,
                Specificity::ZERO,
                None,
                0,
            );
        }
    }
    if let Some(set) = tree {
        let ids = match pseudo {
            None => set.candidates(doc, element),
            Some(_) => pseudo_ids(set),
        };
        match_entries(
            &mut out,
            doc,
            set,
            &ids,
            element,
            Origin::Author,
            depth,
            &tree_ctx,
            bloom,
            how,
        );
    }
    // `:host` rules of the element's own shadow tree.
    if let Some(sr) = doc.shadow_root_of(element) {
        if let Some((d, set)) = stylist.trees.get(&sr) {
            let ids = match pseudo {
                None => set.universal.clone(),
                Some(_) => pseudo_ids(set),
            };
            let ctx = plain.with_shadow_host(Some(element));
            match_entries(
                &mut out,
                doc,
                set,
                &ids,
                element,
                Origin::Author,
                *d,
                &ctx,
                None,
                how,
            );
        }
    }
    // `::slotted()` rules of the shadow tree of the assigned slot.
    if pseudo.is_none() {
        if let Some(slot) = doc.assigned_slot_internal(element) {
            let sroot = tree_root(doc, slot);
            if let Some((d, set)) = stylist.trees.get(&sroot) {
                let ctx = plain.with_shadow_host(host_of(doc, sroot));
                match_entries(
                    &mut out,
                    doc,
                    set,
                    &set.slotted,
                    element,
                    Origin::Author,
                    *d,
                    &ctx,
                    None,
                    matches_slotted,
                );
            }
        }
        // The `style` attribute.
        if let Some(text) = doc.get_attribute(element, "style") {
            let block = DeclarationBlock::parse(text);
            if !block.is_empty() {
                let rank = tree.map_or(0, RuleSet::unlayered);
                candidates_of(
                    &mut out,
                    &Rc::new(block),
                    Origin::Author,
                    depth,
                    true,
                    rank,
                    Specificity::ZERO,
                    None,
                    u32::MAX,
                );
            }
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// One declaration in the cascade of a property.
#[derive(Clone, Copy, Debug)]
pub struct Applied<'a> {
    pub decl: &'a Declaration,
    pub origin: Origin,
    key: Key,
}

/// What the cascade gives a property.
#[derive(Clone, Copy, Debug)]
pub enum Winner<'a> {
    Declared(&'a Declaration),
    /// No declaration is left once `revert` and `revert-layer` rolled
    /// back: the property behaves as `unset`.
    Unset,
}

/// The cascaded declarations of an element, per property, from the
/// highest precedence down.
pub struct Cascaded<'a> {
    map: HashMap<String, Vec<Applied<'a>>>,
}

fn keyword_of(tokens: &[Token]) -> Option<String> {
    let significant: Vec<&Token> = tokens.iter().filter(|t| **t != Token::Whitespace).collect();
    match significant.as_slice() {
        [Token::Ident(k)] => Some(k.to_ascii_lowercase()),
        _ => None,
    }
}

impl<'a> Cascaded<'a> {
    /// The cascade of `candidates` (sorted by [`collect`]).
    pub fn new(candidates: &'a [Candidate]) -> Cascaded<'a> {
        let mut map: HashMap<String, Vec<Applied<'a>>> = HashMap::new();
        for c in candidates {
            for (pos, d) in c.block.declarations.iter().enumerate() {
                if d.important != c.important {
                    continue;
                }
                let key = Key {
                    position: pos as u32,
                    ..c.key
                };
                let applied = Applied {
                    decl: d,
                    origin: c.origin,
                    key,
                };
                for name in expanded_names(d) {
                    map.entry(name).or_default().push(applied);
                }
            }
        }
        for list in map.values_mut() {
            list.sort_by(|a, b| b.key.cmp(&a.key));
        }
        Cascaded { map }
    }

    /// The names that have declarations.
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.map.keys()
    }

    pub fn has(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    /// Moves the declarations of logical properties to the physical
    /// properties they stand for in this writing mode.
    pub fn map_logical(&mut self, writing_mode: &str, rtl: bool) {
        let logical: Vec<String> = self
            .map
            .keys()
            .filter(|n| properties::is_logical(n))
            .cloned()
            .collect();
        for name in logical {
            let Some(physical) = properties::logical_to_physical(&name, writing_mode, rtl) else {
                continue;
            };
            let moved = self.map.remove(&name).unwrap_or_default();
            let list = self.map.entry(physical.to_string()).or_default();
            list.extend(moved);
            list.sort_by(|a, b| b.key.cmp(&a.key));
        }
    }

    /// The winning declaration of `name` after `revert` (back to the
    /// previous origin) and `revert-layer` (back to the previous layer);
    /// `None` without declarations.
    pub fn winner(&self, name: &str) -> Option<Winner<'a>> {
        let list = self.map.get(name)?;
        let mut skip_origin: Option<Origin> = None;
        let mut skip_group = None;
        for a in list {
            if skip_origin == Some(a.origin) || skip_group == Some(a.key.group()) {
                continue;
            }
            let value = if a.decl.pending.is_some() {
                None
            } else {
                keyword_of(&a.decl.value)
            };
            match value.as_deref() {
                Some("revert") => {
                    if a.origin == Origin::UserAgent {
                        return Some(Winner::Unset);
                    }
                    skip_origin = Some(a.origin);
                }
                Some("revert-layer") => skip_group = Some(a.key.group()),
                _ => return Some(Winner::Declared(a.decl)),
            }
        }
        Some(Winner::Unset)
    }
}

/// The longhands a declaration sets: itself, every longhand for `all`,
/// and the longhands of a shorthand kept as one declaration when its
/// value is a CSS-wide keyword (or, for `background`, a lone color, and
/// for `font`, `white-space` and `text-wrap`, any valid value; see
/// [`shorthand_value`]).
fn expanded_names(d: &Declaration) -> Vec<String> {
    if d.name.starts_with("--") || properties::property(&d.name).is_some() {
        return vec![d.name.clone()];
    }
    let wide = keyword_of(&d.value).is_some_and(|k| properties::is_css_wide_keyword(&k));
    if !wide {
        if let Some(parts) = split_shorthand(&d.name, &d.value) {
            return parts.into_iter().map(|(n, _)| n.to_string()).collect();
        }
    }
    if d.name == "background" && !wide && is_lone_color(&d.value) {
        return properties::longhands_of("background")
            .unwrap_or_default()
            .into_iter()
            .map(str::to_string)
            .collect();
    }
    if !wide {
        return Vec::new();
    }
    properties::longhands_of(&d.name)
        .unwrap_or_default()
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// True if `tokens` are one `<color>` (a `background` that only sets
/// the color).
fn is_lone_color(tokens: &[Token]) -> bool {
    matches!(
        super::values::grammar::parse_property("background-color", tokens),
        Some(Some(_))
    )
}

/// The value a declaration gives `longhand` when it was stored under a
/// shorthand name (see [`expanded_names`]); `None` if it gives none.
pub fn shorthand_value(d: &Declaration, longhand: &str) -> Option<Vec<Token>> {
    if d.name == longhand || d.name.starts_with("--") || properties::property(&d.name).is_some() {
        return Some(d.value.clone());
    }
    if keyword_of(&d.value).is_some_and(|k| properties::is_css_wide_keyword(&k)) {
        return Some(d.value.clone());
    }
    if d.name == "background" {
        // A lone color: `background-color` takes it, the other longhands
        // their initial values.
        if longhand == "background-color" {
            return Some(d.value.clone());
        }
        return properties::property(longhand).map(|p| tokenize(p.initial));
    }
    split_shorthand(&d.name, &d.value)?
        .into_iter()
        .find(|(n, _)| *n == longhand)
        .map(|(_, v)| v)
}

/// The top-level component values of `tokens` without whitespace: a
/// function or block with its contents is one component.
fn components(tokens: &[Token]) -> Vec<Vec<Token>> {
    let mut out: Vec<Vec<Token>> = Vec::new();
    let mut depth = 0usize;
    let mut current: Vec<Token> = Vec::new();
    for t in tokens {
        if depth == 0 && *t == Token::Whitespace {
            continue;
        }
        current.push(t.clone());
        match t {
            Token::Function(_) | Token::LeftParen | Token::LeftBracket | Token::LeftBrace => {
                depth += 1
            }
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth = depth.saturating_sub(1)
            }
            _ => {}
        }
        if depth == 0 {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn matches_longhand(name: &str, tokens: &[Token]) -> bool {
    matches!(
        super::values::grammar::parse_property(name, tokens),
        Some(Some(_))
    )
}

fn ident_of(c: &[Token]) -> Option<String> {
    match c {
        [Token::Ident(k)] => Some(k.to_ascii_lowercase()),
        _ => None,
    }
}

/// Every longhand of `shorthand` with its initial value, then the values
/// in `set` in their place.
fn with_initials(
    shorthand: &str,
    set: Vec<(&'static str, Vec<Token>)>,
) -> Vec<(&'static str, Vec<Token>)> {
    let mut out: Vec<(&'static str, Vec<Token>)> = properties::longhands_of(shorthand)
        .unwrap_or_default()
        .into_iter()
        .map(|l| {
            let initial = properties::property(l).map_or("initial", |p| p.initial);
            (l, tokenize(initial))
        })
        .collect();
    for (name, value) in set {
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = value,
            None => out.push((name, value)),
        }
    }
    out
}

/// The longhand values of a shorthand vidrio keeps as one declaration
/// but splits for the cascade: `font` (CSS Fonts 4 §2.8), `white-space`
/// (css-text-4 §3) and `text-wrap` (css-text-4 §6.1). `None` for other
/// shorthands, for values with `var()` and for invalid values.
fn split_shorthand(name: &str, tokens: &[Token]) -> Option<Vec<(&'static str, Vec<Token>)>> {
    if tokens.iter().any(|t| matches!(t, Token::Function(f) if f.eq_ignore_ascii_case("var") || f.eq_ignore_ascii_case("attr") || f.eq_ignore_ascii_case("env"))) {
        return None;
    }
    let comps = components(tokens);
    if comps.is_empty() {
        return None;
    }
    match name {
        "white-space" => {
            let keyword = |k: &str| tokenize(k);
            if let [c] = comps.as_slice() {
                let pair = match ident_of(c).as_deref() {
                    Some("normal") => Some(("collapse", "wrap")),
                    Some("pre") => Some(("preserve", "nowrap")),
                    Some("pre-wrap") => Some(("preserve", "wrap")),
                    Some("pre-line") => Some(("preserve-breaks", "wrap")),
                    _ => None,
                };
                if let Some((collapse, wrap)) = pair {
                    return Some(with_initials(
                        "white-space",
                        vec![
                            ("white-space-collapse", keyword(collapse)),
                            ("text-wrap-mode", keyword(wrap)),
                        ],
                    ));
                }
            }
            let mut set: Vec<(&'static str, Vec<Token>)> = Vec::new();
            for c in &comps {
                let longhand = ["white-space-collapse", "text-wrap-mode"]
                    .into_iter()
                    .find(|l| matches_longhand(l, c))?;
                if set.iter().any(|(n, _)| *n == longhand) {
                    return None;
                }
                set.push((longhand, c.clone()));
            }
            Some(with_initials("white-space", set))
        }
        "text-wrap" => {
            let mut set: Vec<(&'static str, Vec<Token>)> = Vec::new();
            for c in &comps {
                let longhand = ["text-wrap-mode", "text-wrap-style"]
                    .into_iter()
                    .find(|l| matches_longhand(l, c))?;
                if set.iter().any(|(n, _)| *n == longhand) {
                    return None;
                }
                set.push((longhand, c.clone()));
            }
            Some(with_initials("text-wrap", set))
        }
        "font" => split_font(&comps),
        _ => None,
    }
}

/// `font`: `[ <'font-style'> || <font-variant-css2> || <'font-weight'> ||
/// <font-width-css3> ]? <'font-size'> [ / <'line-height'> ]?
/// <'font-family'>#`, or a system font keyword (`sans-serif`, D-15).
fn split_font(comps: &[Vec<Token>]) -> Option<Vec<(&'static str, Vec<Token>)>> {
    if let [c] = comps {
        let k = ident_of(c)?;
        if matches!(
            k.as_str(),
            "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar"
        ) {
            return Some(with_initials(
                "font",
                vec![("font-family", tokenize("sans-serif"))],
            ));
        }
        return None;
    }
    const WIDTHS: &[&str] = &[
        "ultra-condensed",
        "extra-condensed",
        "condensed",
        "semi-condensed",
        "semi-expanded",
        "expanded",
        "extra-expanded",
        "ultra-expanded",
    ];
    let mut set: Vec<(&'static str, Vec<Token>)> = Vec::new();
    let mut i = 0;
    let mut prefix = 0;
    while i < comps.len() && prefix < 4 {
        let c = &comps[i];
        let k = ident_of(c);
        let has = |set: &Vec<(&'static str, Vec<Token>)>, n: &str| set.iter().any(|(x, _)| *x == n);
        if k.as_deref() == Some("normal") {
            i += 1;
            prefix += 1;
            continue;
        }
        if !has(&set, "font-style") && k.as_deref() == Some("oblique") {
            // `oblique <angle>?`
            let mut value = c.clone();
            if let Some(next) = comps.get(i + 1) {
                let mut with_angle = c.clone();
                with_angle.push(Token::Whitespace);
                with_angle.extend(next.iter().cloned());
                if matches_longhand("font-style", &with_angle) {
                    value = with_angle;
                    i += 1;
                }
            }
            set.push(("font-style", value));
        } else if !has(&set, "font-style") && k.as_deref() == Some("italic") {
            set.push(("font-style", c.clone()));
        } else if !has(&set, "font-variant-caps") && k.as_deref() == Some("small-caps") {
            set.push(("font-variant-caps", c.clone()));
        } else if !has(&set, "font-weight")
            && matches_longhand("font-weight", c)
            && !matches_longhand("font-size", c)
        {
            set.push(("font-weight", c.clone()));
        } else if !has(&set, "font-stretch") && k.as_deref().is_some_and(|k| WIDTHS.contains(&k)) {
            set.push(("font-stretch", c.clone()));
        } else {
            break;
        }
        i += 1;
        prefix += 1;
    }
    let size = comps.get(i)?;
    if !matches_longhand("font-size", size) {
        return None;
    }
    set.push(("font-size", size.clone()));
    i += 1;
    if matches!(comps.get(i).map(Vec::as_slice), Some([Token::Delim('/')])) {
        let lh = comps.get(i + 1)?;
        if !matches_longhand("line-height", lh) {
            return None;
        }
        set.push(("line-height", lh.clone()));
        i += 2;
    }
    let mut family: Vec<Token> = Vec::new();
    for c in comps.get(i..)? {
        if !family.is_empty() {
            family.push(Token::Whitespace);
        }
        family.extend(c.iter().cloned());
    }
    if family.is_empty() || !matches_longhand("font-family", &family) {
        return None;
    }
    set.push(("font-family", family));
    Some(with_initials("font", set))
}
