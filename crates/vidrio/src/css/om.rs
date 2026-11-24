//! The CSS object model of style sheets (CSSOM §6.1–§6.4 and the rules of
//! css-conditional, css-cascade, css-nesting, css-fonts,
//! css-properties-values-api, css-cascade-6 and css-page): style sheets
//! and their rule trees built from the parser's raw rules (with the
//! grammar of each rule checked here), "insert a CSS rule" and "remove a
//! CSS rule" with their hierarchy errors, the serialization of rules and
//! selectors, `@supports` conditions (including `selector()`), the
//! `CSS` namespace (`escape`, `supports`, the checks of
//! `registerProperty`).
//!
//! Rules and sheets are shared (`Rc<RefCell<…>>`) so that the bindings
//! keep one object per rule; parents are weak references.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use super::declaration::{DeclContext, DeclarationBlock, serialize_identifier, serialize_string};
use super::media::{self, MediaQueryList};
use super::parser::{
    self, BlockItem, ComponentValue, RawAtRule, RawQualifiedRule, RawRule, non_whitespace,
    split_commas, to_tokens, trim,
};
use super::properties;
use super::selectors::{NamespaceMap, parse_selector_list_with};
use super::tokenizer::{Token, tokenize};
use super::values::custom::{Registration, Syntax};
use crate::dom::Namespace;

pub type SheetRef = Rc<RefCell<StyleSheet>>;
pub type RuleRef = Rc<RefCell<Rule>>;

/// The exceptions of the rule algorithms (their `DOMException` names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleError {
    IndexSize,
    Syntax,
    HierarchyRequest,
    InvalidState,
    NotAllowed,
    Security,
    InvalidModification,
}

impl RuleError {
    /// The `DOMException` name.
    pub fn name(self) -> &'static str {
        match self {
            RuleError::IndexSize => "IndexSizeError",
            RuleError::Syntax => "SyntaxError",
            RuleError::HierarchyRequest => "HierarchyRequestError",
            RuleError::InvalidState => "InvalidStateError",
            RuleError::NotAllowed => "NotAllowedError",
            RuleError::Security => "SecurityError",
            RuleError::InvalidModification => "InvalidModificationError",
        }
    }
}

/// A CSS style sheet (CSSOM §6.1).
#[derive(Debug)]
pub struct StyleSheet {
    pub rules: Vec<RuleRef>,
    pub media: MediaQueryList,
    /// The location (`href`); `None` for inline and constructed sheets.
    pub href: Option<String>,
    /// The base URL `@import` and `url()` resolve against.
    pub base_url: Option<String>,
    pub title: Option<String>,
    pub disabled: bool,
    /// The alternate flag (`rel="alternate stylesheet"`).
    pub alternate: bool,
    /// The constructed flag (`new CSSStyleSheet()`).
    pub constructed: bool,
    /// The disallow modification flag (set while `replace()` runs).
    pub disallow_modification: bool,
    /// The origin-clean flag.
    pub origin_clean: bool,
    /// The `@import` rule that owns the sheet.
    pub owner_rule: Option<Weak<RefCell<Rule>>>,
    /// The parent style sheet (of an imported sheet).
    pub parent_sheet: Option<Weak<RefCell<StyleSheet>>>,
}

/// A CSS rule with its parents.
#[derive(Debug)]
pub struct Rule {
    pub kind: RuleKind,
    pub parent_rule: Option<Weak<RefCell<Rule>>>,
    pub parent_sheet: Option<Weak<RefCell<StyleSheet>>>,
}

/// What a rule is.
#[derive(Debug)]
pub enum RuleKind {
    Style {
        /// The serialized selector list.
        selector: String,
        style: DeclarationBlock,
        rules: Vec<RuleRef>,
    },
    NestedDeclarations {
        style: DeclarationBlock,
    },
    Import {
        /// The URL as written.
        href: String,
        media: MediaQueryList,
        /// `None`: no `layer`; `Some("")`: an anonymous layer.
        layer: Option<String>,
        supports: Option<String>,
        sheet: Option<SheetRef>,
    },
    Media {
        media: MediaQueryList,
        rules: Vec<RuleRef>,
    },
    Supports {
        condition: String,
        matches: bool,
        rules: Vec<RuleRef>,
    },
    LayerBlock {
        name: String,
        rules: Vec<RuleRef>,
    },
    LayerStatement {
        names: Vec<String>,
    },
    Namespace {
        prefix: String,
        uri: String,
    },
    FontFace {
        style: DeclarationBlock,
    },
    Property {
        name: String,
        style: DeclarationBlock,
    },
    Scope {
        start: Option<String>,
        end: Option<String>,
        rules: Vec<RuleRef>,
    },
    Page {
        selector: String,
        style: DeclarationBlock,
        rules: Vec<RuleRef>,
    },
    Margin {
        name: String,
        style: DeclarationBlock,
    },
}

/// The names of the margin rules of `@page` (css-page §4).
pub const MARGIN_RULES: &[&str] = &[
    "top-left-corner",
    "top-left",
    "top-center",
    "top-right",
    "top-right-corner",
    "bottom-left-corner",
    "bottom-left",
    "bottom-center",
    "bottom-right",
    "bottom-right-corner",
    "left-top",
    "left-middle",
    "left-bottom",
    "right-top",
    "right-middle",
    "right-bottom",
];

impl RuleKind {
    /// The child rules of a grouping rule.
    pub fn children(&self) -> Option<&Vec<RuleRef>> {
        match self {
            RuleKind::Style { rules, .. }
            | RuleKind::Media { rules, .. }
            | RuleKind::Supports { rules, .. }
            | RuleKind::LayerBlock { rules, .. }
            | RuleKind::Scope { rules, .. }
            | RuleKind::Page { rules, .. } => Some(rules),
            _ => None,
        }
    }

    pub fn children_mut(&mut self) -> Option<&mut Vec<RuleRef>> {
        match self {
            RuleKind::Style { rules, .. }
            | RuleKind::Media { rules, .. }
            | RuleKind::Supports { rules, .. }
            | RuleKind::LayerBlock { rules, .. }
            | RuleKind::Scope { rules, .. }
            | RuleKind::Page { rules, .. } => Some(rules),
            _ => None,
        }
    }

    /// The declaration block of a rule that has one (`style`).
    pub fn style(&self) -> Option<&DeclarationBlock> {
        match self {
            RuleKind::Style { style, .. }
            | RuleKind::NestedDeclarations { style }
            | RuleKind::FontFace { style }
            | RuleKind::Property { style, .. }
            | RuleKind::Page { style, .. }
            | RuleKind::Margin { style, .. } => Some(style),
            _ => None,
        }
    }

    pub fn style_mut(&mut self) -> Option<&mut DeclarationBlock> {
        match self {
            RuleKind::Style { style, .. }
            | RuleKind::NestedDeclarations { style }
            | RuleKind::FontFace { style }
            | RuleKind::Property { style, .. }
            | RuleKind::Page { style, .. }
            | RuleKind::Margin { style, .. } => Some(style),
            _ => None,
        }
    }

    /// The name of the interface of the rule's object.
    pub fn interface(&self) -> &'static str {
        match self {
            RuleKind::Style { .. } => "CSSStyleRule",
            RuleKind::NestedDeclarations { .. } => "CSSNestedDeclarations",
            RuleKind::Import { .. } => "CSSImportRule",
            RuleKind::Media { .. } => "CSSMediaRule",
            RuleKind::Supports { .. } => "CSSSupportsRule",
            RuleKind::LayerBlock { .. } => "CSSLayerBlockRule",
            RuleKind::LayerStatement { .. } => "CSSLayerStatementRule",
            RuleKind::Namespace { .. } => "CSSNamespaceRule",
            RuleKind::FontFace { .. } => "CSSFontFaceRule",
            RuleKind::Property { .. } => "CSSPropertyRule",
            RuleKind::Scope { .. } => "CSSScopeRule",
            RuleKind::Page { .. } => "CSSPageRule",
            RuleKind::Margin { .. } => "CSSMarginRule",
        }
    }

    /// The historical `type` (CSSOM §6.4.2).
    pub fn type_number(&self) -> u16 {
        match self {
            RuleKind::Style { .. } => 1,
            RuleKind::Import { .. } => 3,
            RuleKind::Media { .. } => 4,
            RuleKind::FontFace { .. } => 5,
            RuleKind::Page { .. } => 6,
            RuleKind::Margin { .. } => 9,
            RuleKind::Namespace { .. } => 10,
            RuleKind::Supports { .. } => 12,
            _ => 0,
        }
    }

    fn is_import(&self) -> bool {
        matches!(self, RuleKind::Import { .. })
    }

    fn is_namespace(&self) -> bool {
        matches!(self, RuleKind::Namespace { .. })
    }
}

/// Creates a rule and makes it the parent of its children.
pub fn new_rule(kind: RuleKind) -> RuleRef {
    let rule = Rc::new(RefCell::new(Rule {
        kind,
        parent_rule: None,
        parent_sheet: None,
    }));
    let children: Vec<RuleRef> = rule.borrow().kind.children().cloned().unwrap_or_default();
    for c in children {
        c.borrow_mut().parent_rule = Some(Rc::downgrade(&rule));
    }
    rule
}

/// Sets the parent style sheet of `rule` and its descendants.
pub fn set_parent_sheet(rule: &RuleRef, sheet: Option<&SheetRef>) {
    rule.borrow_mut().parent_sheet = sheet.map(Rc::downgrade);
    let children: Vec<RuleRef> = rule.borrow().kind.children().cloned().unwrap_or_default();
    for c in children {
        set_parent_sheet(&c, sheet);
    }
}

/// Detaches `rule` from its parents ("remove a CSS rule" step 5).
fn detach(rule: &RuleRef) {
    rule.borrow_mut().parent_rule = None;
    set_parent_sheet(rule, None);
}

// ----- style sheets ----------------------------------------------------------------

impl StyleSheet {
    /// An empty sheet that is not constructed.
    pub fn new() -> StyleSheet {
        StyleSheet {
            rules: Vec::new(),
            media: MediaQueryList::default(),
            href: None,
            base_url: None,
            title: None,
            disabled: false,
            alternate: false,
            constructed: false,
            disallow_modification: false,
            origin_clean: true,
            owner_rule: None,
            parent_sheet: None,
        }
    }

    /// The sheet's `@namespace` declarations.
    pub fn namespaces(&self) -> NamespaceMap {
        namespaces_of(&self.rules)
    }
}

impl Default for StyleSheet {
    fn default() -> StyleSheet {
        StyleSheet::new()
    }
}

/// Parses `text` into a new sheet (an inline or fetched one).
pub fn parse_sheet(text: &str) -> SheetRef {
    let sheet = Rc::new(RefCell::new(StyleSheet::new()));
    replace_rules(&sheet, text, true);
    sheet
}

/// A constructed sheet (`new CSSStyleSheet(options)`).
pub fn constructed_sheet(base_url: Option<String>, media: &str, disabled: bool) -> SheetRef {
    let mut s = StyleSheet::new();
    s.constructed = true;
    s.base_url = base_url;
    s.media = media::parse(media);
    s.disabled = disabled;
    Rc::new(RefCell::new(s))
}

/// Replaces the rules of `sheet` with the rules of `text`; without
/// `allow_imports` (constructed sheets) `@import` rules are dropped.
pub fn replace_rules(sheet: &SheetRef, text: &str, allow_imports: bool) {
    let raw = parser::parse_stylesheet(text);
    let rules = top_level_rules(&raw, allow_imports);
    let old = std::mem::replace(&mut sheet.borrow_mut().rules, rules.clone());
    for r in &old {
        detach(r);
    }
    for r in &rules {
        set_parent_sheet(r, Some(sheet));
    }
}

/// `replaceSync(text)`.
pub fn replace_sync(sheet: &SheetRef, text: &str) -> Result<(), RuleError> {
    {
        let s = sheet.borrow();
        if !s.constructed || s.disallow_modification {
            return Err(RuleError::NotAllowed);
        }
    }
    replace_rules(sheet, text, false);
    Ok(())
}

/// The rules of a whole sheet, with the placement rules of `@import`
/// and `@namespace` (CSS Cascade §6, CSS Namespaces §3).
fn top_level_rules(raw: &[RawRule], allow_imports: bool) -> Vec<RuleRef> {
    let mut out: Vec<RuleRef> = Vec::new();
    // 0: only `@layer` statements and `@import` so far; 1: `@namespace`
    // seen; 2: any other rule seen.
    let mut phase = 0;
    let mut namespaces = NamespaceMap::default();
    for r in raw {
        if let RawRule::At(a) = r {
            if a.name.eq_ignore_ascii_case("charset") {
                continue;
            }
        }
        let Some(kind) = rule_from_raw(r, Ctx::TopLevel, &namespaces) else {
            continue;
        };
        match &kind {
            RuleKind::Import { .. } => {
                if phase > 0 || !allow_imports {
                    continue;
                }
            }
            RuleKind::Namespace { prefix, uri } => {
                if phase > 1 {
                    continue;
                }
                phase = 1;
                add_namespace(&mut namespaces, prefix, uri);
            }
            RuleKind::LayerStatement { .. } if phase == 0 => {}
            _ => phase = 2,
        }
        out.push(new_rule(kind));
    }
    out
}

fn add_namespace(map: &mut NamespaceMap, prefix: &str, uri: &str) {
    let ns = Namespace::from_url(uri);
    if prefix.is_empty() {
        map.default = Some(ns);
    } else {
        map.prefixes.retain(|(p, _)| p != prefix);
        map.prefixes.push((prefix.to_string(), ns));
    }
}

fn namespaces_of(rules: &[RuleRef]) -> NamespaceMap {
    let mut map = NamespaceMap::default();
    for r in rules {
        if let RuleKind::Namespace { prefix, uri } = &r.borrow().kind {
            add_namespace(&mut map, prefix, uri);
        }
    }
    map
}

// ----- building rules from raw rules -----------------------------------------------

/// Where a rule appears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctx {
    /// Directly in a style sheet.
    TopLevel,
    /// In a conditional or layer rule outside style rules.
    Group,
    /// In a style rule, or in a group rule inside one, or in `@scope`:
    /// relative selectors and nested declarations.
    Nested,
    /// In `@page`: margin rules.
    Page,
}

fn ident_is(v: &ComponentValue, word: &str) -> bool {
    v.ident().is_some_and(|s| s.eq_ignore_ascii_case(word))
}

/// The CSS text of component values with whitespace runs collapsed.
fn collapsed(values: &[ComponentValue]) -> String {
    let mut tokens = Vec::new();
    for t in to_tokens(trim(values)) {
        if t == Token::Whitespace && tokens.last() == Some(&Token::Whitespace) {
            continue;
        }
        tokens.push(t);
    }
    super::declaration::serialize_tokens(&tokens)
}

fn rule_from_raw(raw: &RawRule, ctx: Ctx, ns: &NamespaceMap) -> Option<RuleKind> {
    match raw {
        RawRule::Qualified(q) => {
            if ctx == Ctx::Page {
                return None;
            }
            style_rule(q, ctx, ns)
        }
        RawRule::At(a) => at_rule(a, ctx, ns),
    }
}

/// The child rules of a group rule's block in `ctx`; declarations become
/// nested declarations rules in a nested context.
fn child_rules(block: &[BlockItem], ctx: Ctx, ns: &NamespaceMap) -> Vec<RuleRef> {
    let mut out = Vec::new();
    for item in block {
        match item {
            BlockItem::Rule(r) => {
                if let Some(k) = rule_from_raw(r, ctx, ns) {
                    if !matches!(k, RuleKind::Import { .. } | RuleKind::Namespace { .. }) {
                        out.push(new_rule(k));
                    }
                }
            }
            BlockItem::Declarations(decls) if ctx == Ctx::Nested => {
                let style = block_of(DeclContext::Style, decls);
                if !style.is_empty() {
                    out.push(new_rule(RuleKind::NestedDeclarations { style }));
                }
            }
            BlockItem::Declarations(_) => {}
        }
    }
    out
}

fn block_of(context: DeclContext, decls: &[parser::RawDeclaration]) -> DeclarationBlock {
    let mut block = DeclarationBlock::new(context);
    for d in decls {
        block.push_raw(&d.name, &to_tokens(&d.value), d.important);
    }
    block
}

fn style_rule(q: &RawQualifiedRule, ctx: Ctx, ns: &NamespaceMap) -> Option<RuleKind> {
    let tokens = to_tokens(trim(&q.prelude));
    let nested = ctx == Ctx::Nested;
    if !selector_valid(&tokens, nested, ns) {
        return None;
    }
    let selector = serialize_selector(&tokens);
    let mut style = DeclarationBlock::new(DeclContext::Style);
    let mut rest: &[BlockItem] = &q.block;
    if let Some(BlockItem::Declarations(decls)) = q.block.first() {
        style = block_of(DeclContext::Style, decls);
        rest = &q.block[1..];
    }
    let rules = child_rules(rest, Ctx::Nested, ns);
    Some(RuleKind::Style {
        selector,
        style,
        rules,
    })
}

fn at_rule(a: &RawAtRule, ctx: Ctx, ns: &NamespaceMap) -> Option<RuleKind> {
    let name = a.name.to_ascii_lowercase();
    let inner = |c: Ctx| {
        if c == Ctx::Nested {
            Ctx::Nested
        } else {
            Ctx::Group
        }
    };
    if ctx == Ctx::Page {
        // Only margin rules live in `@page`.
        if !MARGIN_RULES.contains(&name.as_str()) || !trim(&a.prelude).is_empty() {
            return None;
        }
        let block = a.block.as_ref()?;
        let decls: Vec<parser::RawDeclaration> = parser::declarations_of(block)
            .into_iter()
            .cloned()
            .collect();
        return Some(RuleKind::Margin {
            name,
            style: block_of(DeclContext::Page, &decls),
        });
    }
    match name.as_str() {
        "import" if a.block.is_none() => import_rule(&a.prelude),
        "namespace" if a.block.is_none() => namespace_rule(&a.prelude),
        "media" => {
            let block = a.block.as_ref()?;
            Some(RuleKind::Media {
                media: media::parse_values(trim(&a.prelude)),
                rules: child_rules(block, inner(ctx), ns),
            })
        }
        "supports" => {
            let block = a.block.as_ref()?;
            let matches = supports_condition(trim(&a.prelude), ns)?;
            Some(RuleKind::Supports {
                condition: collapsed(&a.prelude),
                matches,
                rules: child_rules(block, inner(ctx), ns),
            })
        }
        "layer" => {
            let parts = split_commas(trim(&a.prelude));
            match &a.block {
                None => {
                    let names = parts
                        .iter()
                        .map(|p| layer_name(p))
                        .collect::<Option<Vec<String>>>()?;
                    if names.iter().any(String::is_empty) {
                        return None;
                    }
                    Some(RuleKind::LayerStatement { names })
                }
                Some(block) => {
                    if parts.len() != 1 {
                        return None;
                    }
                    let name = if trim(parts[0]).is_empty() {
                        String::new()
                    } else {
                        let n = layer_name(parts[0])?;
                        if n.is_empty() {
                            return None;
                        }
                        n
                    };
                    Some(RuleKind::LayerBlock {
                        name,
                        rules: child_rules(block, inner(ctx), ns),
                    })
                }
            }
        }
        "font-face" if ctx != Ctx::Nested => {
            if !trim(&a.prelude).is_empty() {
                return None;
            }
            let block = a.block.as_ref()?;
            let decls: Vec<parser::RawDeclaration> = parser::declarations_of(block)
                .into_iter()
                .cloned()
                .collect();
            Some(RuleKind::FontFace {
                style: block_of(DeclContext::FontFace, &decls),
            })
        }
        "property" if ctx != Ctx::Nested => property_rule(a),
        "scope" => {
            let block = a.block.as_ref()?;
            let (start, end) = scope_prelude(trim(&a.prelude), ns)?;
            Some(RuleKind::Scope {
                start,
                end,
                rules: child_rules(block, Ctx::Nested, ns),
            })
        }
        "page" if ctx != Ctx::Nested => {
            let block = a.block.as_ref()?;
            let selector = page_selector(trim(&a.prelude))?;
            let mut decls = Vec::new();
            let mut rules = Vec::new();
            for item in block {
                match item {
                    BlockItem::Declarations(d) => decls.extend(d.iter().cloned()),
                    BlockItem::Rule(r) => {
                        if let Some(k) = rule_from_raw(r, Ctx::Page, ns) {
                            rules.push(new_rule(k));
                        }
                    }
                }
            }
            Some(RuleKind::Page {
                selector,
                style: block_of(DeclContext::Page, &decls),
                rules,
            })
        }
        _ => None,
    }
}

/// The URL of an `@import` or `@namespace` prelude component.
fn url_of(v: &ComponentValue) -> Option<String> {
    match v {
        ComponentValue::Token(Token::Url(u)) | ComponentValue::Token(Token::String(u)) => {
            Some(u.clone())
        }
        ComponentValue::Function(f, args) if f.eq_ignore_ascii_case("url") => {
            match non_whitespace(args).as_slice() {
                [ComponentValue::Token(Token::String(u))] => Some(u.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

fn import_rule(prelude: &[ComponentValue]) -> Option<RuleKind> {
    let v = trim(prelude);
    let first = v.iter().position(|c| !c.is_whitespace())?;
    let href = url_of(&v[first])?;
    let mut rest = trim(&v[first + 1..]);
    let mut layer = None;
    if let Some(c) = rest.first() {
        if ident_is(c, "layer") {
            layer = Some(String::new());
            rest = trim(&rest[1..]);
        } else if let ComponentValue::Function(f, args) = c {
            if f.eq_ignore_ascii_case("layer") {
                let n = layer_name(args)?;
                if n.is_empty() {
                    return None;
                }
                layer = Some(n);
                rest = trim(&rest[1..]);
            }
        }
    }
    let mut supports = None;
    if let Some(ComponentValue::Function(f, args)) = rest.first() {
        if f.eq_ignore_ascii_case("supports") {
            let args = trim(args);
            let valid = supports_condition(args, &NamespaceMap::default()).is_some()
                || parser::declaration_from_values(args).is_some();
            if !valid {
                return None;
            }
            supports = Some(collapsed(args));
            rest = trim(&rest[1..]);
        }
    }
    Some(RuleKind::Import {
        href,
        media: media::parse_values(rest),
        layer,
        supports,
        sheet: None,
    })
}

fn namespace_rule(prelude: &[ComponentValue]) -> Option<RuleKind> {
    let v = non_whitespace(prelude);
    match v.as_slice() {
        [url] => Some(RuleKind::Namespace {
            prefix: String::new(),
            uri: url_of(url)?,
        }),
        [ComponentValue::Token(Token::Ident(prefix)), url] => Some(RuleKind::Namespace {
            prefix: prefix.clone(),
            uri: url_of(url)?,
        }),
        _ => None,
    }
}

/// `<layer-name> = <ident> [ '.' <ident> ]*` without whitespace inside;
/// `Some("")` for empty input.
fn layer_name(v: &[ComponentValue]) -> Option<String> {
    let v = trim(v);
    let mut out = String::new();
    let mut want_ident = true;
    for c in v {
        match c {
            ComponentValue::Token(Token::Ident(s)) if want_ident => {
                if properties::is_css_wide_keyword(s) || s.eq_ignore_ascii_case("default") {
                    return None;
                }
                out.push_str(&serialize_identifier(s));
                want_ident = false;
            }
            ComponentValue::Token(Token::Delim('.')) if !want_ident => {
                out.push('.');
                want_ident = true;
            }
            _ => return None,
        }
    }
    if want_ident && !out.is_empty() {
        return None;
    }
    Some(out)
}

fn property_rule(a: &RawAtRule) -> Option<RuleKind> {
    let name = match non_whitespace(&a.prelude).as_slice() {
        [ComponentValue::Token(Token::Ident(n))] if n.starts_with("--") && n.len() > 2 => n.clone(),
        _ => return None,
    };
    let block = a.block.as_ref()?;
    let decls: Vec<parser::RawDeclaration> = parser::declarations_of(block)
        .into_iter()
        .cloned()
        .collect();
    let style = block_of(DeclContext::Property, &decls);
    let syntax = property_syntax(&style)?;
    property_inherits(&style)?;
    let initial = style.property_value("initial-value");
    let initial = (!initial.is_empty()).then_some(initial);
    if check_registration(&syntax, initial.as_deref()).is_err() {
        return None;
    }
    Some(RuleKind::Property { name, style })
}

/// The `syntax` descriptor of `@property` (a string).
pub fn property_syntax(style: &DeclarationBlock) -> Option<String> {
    let tokens = tokenize(&style.property_value("syntax"));
    match tokens.as_slice() {
        [Token::String(s)] => Some(s.clone()),
        _ => None,
    }
}

/// The `inherits` descriptor of `@property`.
pub fn property_inherits(style: &DeclarationBlock) -> Option<bool> {
    let v = style.property_value("inherits");
    if v.eq_ignore_ascii_case("true") {
        Some(true)
    } else if v.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn scope_prelude(
    v: &[ComponentValue],
    ns: &NamespaceMap,
) -> Option<(Option<String>, Option<String>)> {
    let parts = non_whitespace(v);
    match parts.as_slice() {
        [] => Some((None, None)),
        [s] => Some((Some(scope_selector(s, ns)?), None)),
        [to, e] if ident_is(to, "to") => Some((None, Some(scope_selector(e, ns)?))),
        [s, to, e] if ident_is(to, "to") => {
            Some((Some(scope_selector(s, ns)?), Some(scope_selector(e, ns)?)))
        }
        _ => None,
    }
}

/// A `(<scope-start>)` or `(<scope-end>)` block, serialized.
fn scope_selector(c: &ComponentValue, ns: &NamespaceMap) -> Option<String> {
    match c {
        ComponentValue::Block('(', inner) => {
            let tokens = to_tokens(trim(inner));
            selector_valid(&tokens, true, ns).then(|| serialize_selector(&tokens))
        }
        _ => None,
    }
}

/// A page selector list (css-page §3.1), serialized; `Some("")` if empty.
fn page_selector(v: &[ComponentValue]) -> Option<String> {
    let mut out = Vec::new();
    for part in split_commas(v) {
        let part = trim(part);
        if part.is_empty() {
            return None;
        }
        let mut s = String::new();
        let mut i = 0;
        if let Some(ComponentValue::Token(Token::Ident(n))) = part.first() {
            s.push_str(&serialize_identifier(n));
            i = 1;
        }
        while i < part.len() {
            match (&part[i], part.get(i + 1)) {
                (
                    ComponentValue::Token(Token::Colon),
                    Some(ComponentValue::Token(Token::Ident(p))),
                ) => {
                    let p = p.to_ascii_lowercase();
                    if !matches!(p.as_str(), "first" | "left" | "right" | "blank") {
                        return None;
                    }
                    s.push(':');
                    s.push_str(&p);
                    i += 2;
                }
                _ => return None,
            }
        }
        out.push(s);
    }
    if out.len() == 1 && out[0].is_empty() {
        return Some(String::new());
    }
    Some(out.join(", "))
}

// ----- selectors -------------------------------------------------------------------

/// Splits tokens at top-level commas.
fn split_token_commas(tokens: &[Token]) -> Vec<&[Token]> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, t) in tokens.iter().enumerate() {
        match t {
            Token::Function(_) | Token::LeftParen | Token::LeftBracket | Token::LeftBrace => {
                depth += 1
            }
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth = depth.saturating_sub(1)
            }
            Token::Comma if depth == 0 => {
                parts.push(&tokens[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&tokens[start..]);
    parts
}

fn trim_tokens(mut t: &[Token]) -> &[Token] {
    while t.first() == Some(&Token::Whitespace) {
        t = &t[1..];
    }
    while t.last() == Some(&Token::Whitespace) {
        t = &t[..t.len() - 1];
    }
    t
}

fn is_combinator(t: &Token) -> bool {
    matches!(t, Token::Delim('>' | '+' | '~'))
}

/// Whether `tokens` is a valid selector list for a style rule: the
/// nesting selector `&` is checked as a compound that matches like it,
/// and in a nested context a selector may start with a combinator.
pub fn selector_valid(tokens: &[Token], nested: bool, ns: &NamespaceMap) -> bool {
    let mut text_parts = Vec::new();
    for part in split_token_commas(tokens) {
        let part = trim_tokens(part);
        if part.is_empty() {
            return false;
        }
        let mut out: Vec<Token> = Vec::new();
        if nested && is_combinator(&part[0]) {
            out.extend(is_star());
            out.push(Token::Whitespace);
        }
        for t in part {
            if *t == Token::Delim('&') {
                out.extend(is_star());
            } else {
                out.push(t.clone());
            }
        }
        text_parts.push(super::declaration::serialize_tokens(&out));
    }
    parse_selector_list_with(&text_parts.join(", "), ns).is_ok()
}

/// `:is(*)`, a stand-in for the nesting selector.
fn is_star() -> [Token; 4] {
    [
        Token::Colon,
        Token::Function("is".to_string()),
        Token::Delim('*'),
        Token::RightParen,
    ]
}

/// "Serialize a group of selectors" (CSSOM §6.6.1) from a valid
/// selector's tokens: whitespace collapsed, combinators and commas with
/// their canonical spacing, pseudo-class names lowercased and attribute
/// values as strings.
pub fn serialize_selector(tokens: &[Token]) -> String {
    let parts: Vec<String> = split_token_commas(tokens)
        .into_iter()
        .map(|p| serialize_complex(trim_tokens(p)))
        .collect();
    parts.join(", ")
}

fn serialize_complex(tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        match t {
            Token::Whitespace => {
                // A descendant combinator unless next to another one.
                let next = tokens[i + 1..].iter().find(|t| **t != Token::Whitespace);
                let prev_comb = out.ends_with("> ") || out.ends_with("+ ") || out.ends_with("~ ");
                if !prev_comb && !next.is_some_and(is_combinator) && !out.is_empty() {
                    out.push(' ');
                }
                i += 1;
            }
            t if is_combinator(t) => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                if let Token::Delim(c) = t {
                    out.push(*c);
                }
                out.push(' ');
                i += 1;
            }
            Token::Colon => {
                out.push(':');
                i += 1;
                if tokens.get(i) == Some(&Token::Colon) {
                    out.push(':');
                    i += 1;
                }
                match tokens.get(i) {
                    Some(Token::Ident(name)) => {
                        out.push_str(&serialize_identifier(&name.to_ascii_lowercase()));
                        i += 1;
                    }
                    Some(Token::Function(name)) => {
                        let lower = name.to_ascii_lowercase();
                        let end = matching_paren(tokens, i);
                        let inner = trim_tokens(&tokens[i + 1..end.min(tokens.len())]);
                        out.push_str(&serialize_identifier(&lower));
                        out.push('(');
                        if lower.starts_with("nth-") {
                            out.push_str(&serialize_nth(inner));
                        } else if matches!(
                            lower.as_str(),
                            "is" | "where" | "not" | "has" | "host" | "host-context" | "slotted"
                        ) {
                            out.push_str(&serialize_selector(inner));
                        } else {
                            out.push_str(&super::declaration::serialize_tokens(inner));
                        }
                        out.push(')');
                        i = end + 1;
                    }
                    _ => {}
                }
            }
            Token::LeftBracket => {
                let end = matching_paren(tokens, i);
                out.push_str(&serialize_attribute(&tokens[i + 1..end.min(tokens.len())]));
                i = end + 1;
            }
            t => {
                out.push_str(&super::declaration::serialize_tokens(std::slice::from_ref(
                    t,
                )));
                i += 1;
            }
        }
    }
    out.trim_end().to_string()
}

/// The index of the token that closes the block or function opened at
/// `open` (`tokens.len()` if unclosed).
fn matching_paren(tokens: &[Token], open: usize) -> usize {
    let mut depth = 0usize;
    for (i, t) in tokens.iter().enumerate().skip(open) {
        match t {
            Token::Function(_) | Token::LeftParen | Token::LeftBracket | Token::LeftBrace => {
                depth += 1
            }
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    tokens.len()
}

/// `An+B` arguments with their whitespace removed, and an `of S` part.
fn serialize_nth(tokens: &[Token]) -> String {
    let of = tokens
        .iter()
        .position(|t| matches!(t, Token::Ident(s) if s.eq_ignore_ascii_case("of")));
    let (anb, rest) = match of {
        Some(i) => (&tokens[..i], Some(&tokens[i + 1..])),
        None => (tokens, None),
    };
    let anb: Vec<Token> = anb
        .iter()
        .filter(|t| **t != Token::Whitespace)
        .cloned()
        .collect();
    let mut s = super::declaration::serialize_tokens(&anb)
        .replace("/**/", "")
        .to_ascii_lowercase();
    if let Some(rest) = rest {
        s.push_str(" of ");
        s.push_str(&serialize_selector(trim_tokens(rest)));
    }
    s
}

/// `[ns|name op "value" flag]`.
fn serialize_attribute(tokens: &[Token]) -> String {
    let t: Vec<&Token> = tokens.iter().filter(|t| **t != Token::Whitespace).collect();
    let mut out = String::from("[");
    let mut i = 0;
    // The attribute name with its namespace prefix.
    while i < t.len() {
        match t[i] {
            Token::Delim('|') if t.get(i + 1) == Some(&&Token::Delim('=')) => break,
            Token::Delim('|') => out.push('|'),
            Token::Delim('*') => out.push('*'),
            Token::Ident(s) => out.push_str(&serialize_identifier(s)),
            _ => break,
        }
        i += 1;
    }
    if i < t.len() {
        // The operator.
        while i < t.len() {
            match t[i] {
                Token::Delim('=') => {
                    out.push('=');
                    i += 1;
                    break;
                }
                Token::Delim(c) => out.push(*c),
                _ => break,
            }
            i += 1;
        }
        match t.get(i) {
            Some(Token::Ident(v)) | Some(Token::String(v)) => out.push_str(&serialize_string(v)),
            Some(other) => out.push_str(&super::declaration::serialize_tokens(&[(*other).clone()])),
            None => {}
        }
        i += 1;
        if let Some(Token::Ident(flag)) = t.get(i) {
            out.push(' ');
            out.push_str(&flag.to_ascii_lowercase());
        }
    }
    out.push(']');
    out
}

// ----- @supports ---------------------------------------------------------------------

/// Evaluates a `<supports-condition>`; `None` if it does not parse.
pub fn supports_condition(v: &[ComponentValue], ns: &NamespaceMap) -> Option<bool> {
    let parts = non_whitespace(v);
    let first = parts.first()?;
    if ident_is(first, "not") {
        return match parts.as_slice() {
            [_, inner] => supports_in_parens(inner, ns).map(|b| !b),
            _ => None,
        };
    }
    let mut result = supports_in_parens(first, ns)?;
    let mut op: Option<&str> = None;
    let mut i = 1;
    while i < parts.len() {
        let word = parts[i].ident()?.to_ascii_lowercase();
        if word != "and" && word != "or" {
            return None;
        }
        match op {
            Some(o) if o != word => return None,
            _ => {}
        }
        op = Some(if word == "and" { "and" } else { "or" });
        let next = supports_in_parens(parts.get(i + 1)?, ns)?;
        result = if word == "and" {
            result && next
        } else {
            result || next
        };
        i += 2;
    }
    // `and` and `or` must be followed by whitespace: `and(` is a function.
    Some(result)
}

fn supports_in_parens(v: &ComponentValue, ns: &NamespaceMap) -> Option<bool> {
    match v {
        ComponentValue::Block('(', inner) => {
            if let Some(b) = supports_condition(inner, ns) {
                return Some(b);
            }
            if let Some(d) = parser::declaration_from_values(trim(inner)) {
                return Some(supports_declaration(&d.name, &to_tokens(&d.value)));
            }
            // `<general-enclosed>`: valid, false.
            Some(false)
        }
        ComponentValue::Function(f, args) => {
            if f.eq_ignore_ascii_case("selector") {
                let tokens = to_tokens(trim(args));
                return Some(!tokens.is_empty() && selector_supported(&tokens, ns));
            }
            Some(false)
        }
        _ => None,
    }
}

/// `selector()`: a complex selector that parses (forgiving lists do not
/// count: a selector list or `&` is not a single complex selector).
fn selector_supported(tokens: &[Token], ns: &NamespaceMap) -> bool {
    if split_token_commas(tokens).len() != 1 || tokens.contains(&Token::Delim('&')) {
        return false;
    }
    let text = super::declaration::serialize_tokens(tokens);
    parse_selector_list_with(&text, ns).is_ok()
}

/// Whether `name: value` is a supported declaration.
pub fn supports_declaration(name: &str, value: &[Token]) -> bool {
    let mut block = DeclarationBlock::new(DeclContext::Style);
    block.push_raw(name, value, false);
    !block.is_empty()
}

/// `CSS.supports(property, value)`.
pub fn css_supports_property(property: &str, value: &str) -> bool {
    if property.trim() != property || property.is_empty() {
        return false;
    }
    let tokens = tokenize(value);
    if tokens.iter().any(|t| *t == Token::Delim('!')) {
        // `!important` is not part of a value.
        return false;
    }
    let mut block = DeclarationBlock::new(DeclContext::Style);
    block.push_raw(property, &tokens, false);
    !block.is_empty()
}

/// `CSS.supports(conditionText)`.
pub fn css_supports_condition(text: &str) -> bool {
    let ns = NamespaceMap::default();
    let values = parser::parse_component_values(text);
    if let Some(b) = supports_condition(trim(&values), &ns) {
        return b;
    }
    // A declaration without its parentheses.
    let wrapped = parser::parse_component_values(&format!("({text})"));
    supports_condition(trim(&wrapped), &ns).unwrap_or(false)
}

/// `CSS.escape(ident)`.
pub fn css_escape(ident: &str) -> String {
    serialize_identifier(ident)
}

// ----- registered custom properties --------------------------------------------------

/// Whether `syntax` is a valid syntax string (css-properties-values-api
/// §5.4).
pub fn syntax_valid(syntax: &str) -> bool {
    Syntax::parse(syntax).is_some()
}

/// The checks of `registerProperty` and `@property` on `syntax` and the
/// initial value (a match of the syntax that is computationally
/// independent): `SyntaxError` when they fail.
pub fn check_registration(syntax: &str, initial: Option<&str>) -> Result<(), RuleError> {
    Registration::new("--registration", syntax, false, initial)
        .map(|_| ())
        .map_err(|_| RuleError::Syntax)
}

// ----- inserting and removing rules ------------------------------------------------

/// The rule list a rule algorithm runs on.
#[derive(Clone)]
pub enum Owner {
    Sheet(SheetRef),
    Rule(RuleRef),
}

impl Owner {
    fn rules(&self) -> Vec<RuleRef> {
        match self {
            Owner::Sheet(s) => s.borrow().rules.clone(),
            Owner::Rule(r) => r.borrow().kind.children().cloned().unwrap_or_default(),
        }
    }

    /// The number of rules.
    pub fn len(&self) -> usize {
        match self {
            Owner::Sheet(s) => s.borrow().rules.len(),
            Owner::Rule(r) => r.borrow().kind.children().map_or(0, Vec::len),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The rule at `index`.
    pub fn item(&self, index: usize) -> Option<RuleRef> {
        self.rules().get(index).cloned()
    }

    fn sheet(&self) -> Option<SheetRef> {
        match self {
            Owner::Sheet(s) => Some(s.clone()),
            Owner::Rule(r) => r.borrow().parent_sheet.as_ref().and_then(Weak::upgrade),
        }
    }
}

/// Whether `rule` or one of its ancestors is a style rule or `@scope`
/// (its children are nested).
fn in_nested_context(rule: &RuleRef) -> bool {
    let mut next = Some(rule.clone());
    while let Some(r) = next {
        if matches!(
            r.borrow().kind,
            RuleKind::Style { .. } | RuleKind::Scope { .. }
        ) {
            return true;
        }
        next = r.borrow().parent_rule.as_ref().and_then(Weak::upgrade);
    }
    false
}

/// The `@import`/`@namespace` placement rules of a sheet's rule list.
fn valid_order(kinds: &[&RuleKind]) -> bool {
    let mut phase = 0;
    for k in kinds {
        match k {
            RuleKind::Import { .. } => {
                if phase > 0 {
                    return false;
                }
            }
            RuleKind::Namespace { .. } => {
                if phase > 1 {
                    return false;
                }
                phase = 1;
            }
            RuleKind::LayerStatement { .. } if phase == 0 => {}
            _ => phase = 2,
        }
    }
    true
}

/// "Insert a CSS rule" `text` into `owner` at `index` (CSSOM §6.4.1),
/// with the checks of `CSSStyleSheet.insertRule` and
/// `CSSGroupingRule.insertRule`.
pub fn insert_rule(owner: &Owner, text: &str, index: usize) -> Result<usize, RuleError> {
    if let Owner::Sheet(s) = owner {
        let s = s.borrow();
        if !s.origin_clean {
            return Err(RuleError::Security);
        }
        if s.disallow_modification {
            return Err(RuleError::NotAllowed);
        }
    }
    let ctx = match owner {
        Owner::Sheet(_) => Ctx::TopLevel,
        Owner::Rule(r) => {
            if matches!(r.borrow().kind, RuleKind::Page { .. }) {
                Ctx::Page
            } else if in_nested_context(r) {
                Ctx::Nested
            } else {
                Ctx::Group
            }
        }
    };
    let rules = owner.rules();
    if index > rules.len() {
        return Err(RuleError::IndexSize);
    }
    let ns = match owner.sheet() {
        Some(s) => s.borrow().namespaces(),
        None => NamespaceMap::default(),
    };
    let nested = ctx == Ctx::Nested;
    let parsed = parser::parse_rule(text, nested)
        .ok()
        .filter(|r| !matches!(r, RawRule::At(a) if a.name.eq_ignore_ascii_case("charset")))
        .and_then(|r| rule_from_raw(&r, ctx, &ns));
    let kind = match parsed {
        Some(k) => k,
        None if nested => {
            // "Parse a CSS declaration block" for nested declarations.
            let style = DeclarationBlock::parse(text);
            if style.is_empty() {
                return Err(RuleError::Syntax);
            }
            RuleKind::NestedDeclarations { style }
        }
        None => return Err(RuleError::Syntax),
    };
    if let Owner::Sheet(s) = owner {
        if kind.is_import() && s.borrow().constructed {
            return Err(RuleError::Syntax);
        }
    }
    // Step 5: the hierarchy constraints.
    match ctx {
        Ctx::TopLevel => {
            let borrowed: Vec<std::cell::Ref<Rule>> = rules.iter().map(|r| r.borrow()).collect();
            let mut kinds: Vec<&RuleKind> = borrowed.iter().map(|r| &r.kind).collect();
            kinds.insert(index, &kind);
            if !valid_order(&kinds) {
                return Err(RuleError::HierarchyRequest);
            }
        }
        _ => {
            if kind.is_import() || kind.is_namespace() {
                return Err(RuleError::HierarchyRequest);
            }
        }
    }
    // Step 6.
    if kind.is_namespace()
        && rules.iter().any(|r| {
            let k = &r.borrow().kind;
            !k.is_import() && !k.is_namespace()
        })
    {
        return Err(RuleError::InvalidState);
    }
    let rule = new_rule(kind);
    match owner {
        Owner::Sheet(s) => {
            s.borrow_mut().rules.insert(index, rule.clone());
            set_parent_sheet(&rule, Some(s));
        }
        Owner::Rule(parent) => {
            if let Some(list) = parent.borrow_mut().kind.children_mut() {
                list.insert(index, rule.clone());
            }
            rule.borrow_mut().parent_rule = Some(Rc::downgrade(parent));
            let sheet = parent
                .borrow()
                .parent_sheet
                .as_ref()
                .and_then(Weak::upgrade);
            set_parent_sheet(&rule, sheet.as_ref());
        }
    }
    Ok(index)
}

/// "Remove a CSS rule" at `index` of `owner` (CSSOM §6.4.2).
pub fn delete_rule(owner: &Owner, index: usize) -> Result<(), RuleError> {
    if let Owner::Sheet(s) = owner {
        let s = s.borrow();
        if !s.origin_clean {
            return Err(RuleError::Security);
        }
        if s.disallow_modification {
            return Err(RuleError::NotAllowed);
        }
    }
    let rules = owner.rules();
    let Some(old) = rules.get(index).cloned() else {
        return Err(RuleError::IndexSize);
    };
    if old.borrow().kind.is_namespace()
        && rules.iter().any(|r| {
            let k = &r.borrow().kind;
            !k.is_import() && !k.is_namespace()
        })
    {
        return Err(RuleError::InvalidState);
    }
    match owner {
        Owner::Sheet(s) => {
            s.borrow_mut().rules.remove(index);
        }
        Owner::Rule(parent) => {
            if let Some(list) = parent.borrow_mut().kind.children_mut() {
                list.remove(index);
            }
        }
    }
    detach(&old);
    Ok(())
}

/// Sets `selectorText` of a style or page rule; an invalid selector
/// changes nothing.
pub fn set_selector_text(rule: &RuleRef, text: &str) {
    let nested = rule
        .borrow()
        .parent_rule
        .as_ref()
        .and_then(Weak::upgrade)
        .is_some_and(|p| in_nested_context(&p));
    let ns = rule
        .borrow()
        .parent_sheet
        .as_ref()
        .and_then(Weak::upgrade)
        .map(|s| s.borrow().namespaces())
        .unwrap_or_default();
    let mut r = rule.borrow_mut();
    match &mut r.kind {
        RuleKind::Style { selector, .. } => {
            let tokens: Vec<Token> = tokenize(text);
            let tokens = trim_tokens(&tokens);
            if selector_valid(tokens, nested, &ns) {
                *selector = serialize_selector(tokens);
            }
        }
        RuleKind::Page { selector, .. } => {
            let values = parser::parse_component_values(text);
            if let Some(s) = page_selector(trim(&values)) {
                *selector = s;
            }
        }
        _ => {}
    }
}

// ----- serialization ---------------------------------------------------------------

fn indent(s: &str) -> String {
    s.replace('\n', "\n  ")
}

fn group(head: String, rules: &[RuleRef]) -> String {
    let mut s = head;
    s.push_str(" {");
    for r in rules {
        s.push_str("\n  ");
        s.push_str(&indent(&serialize_rule(&r.borrow())));
    }
    s.push_str("\n}");
    s
}

fn declarations_rule(head: &str, style: &DeclarationBlock) -> String {
    let decls = style.serialize();
    if decls.is_empty() {
        format!("{head} {{ }}")
    } else {
        format!("{head} {{ {decls} }}")
    }
}

fn url_text(href: &str) -> String {
    format!("url({})", serialize_string(href))
}

/// "Serialize a CSS rule" (CSSOM §6.4.3).
pub fn serialize_rule(rule: &Rule) -> String {
    match &rule.kind {
        RuleKind::Style {
            selector,
            style,
            rules,
        } => {
            let decls = style.serialize();
            if rules.is_empty() {
                return declarations_rule(selector, style);
            }
            let mut s = format!("{selector} {{");
            if !decls.is_empty() {
                s.push_str("\n  ");
                s.push_str(&decls);
            }
            for r in rules {
                s.push_str("\n  ");
                s.push_str(&indent(&serialize_rule(&r.borrow())));
            }
            s.push_str("\n}");
            s
        }
        RuleKind::NestedDeclarations { style } => style.serialize(),
        RuleKind::Import {
            href,
            media,
            layer,
            supports,
            ..
        } => {
            let mut s = format!("@import {}", url_text(href));
            match layer.as_deref() {
                Some("") => s.push_str(" layer"),
                Some(n) => s.push_str(&format!(" layer({n})")),
                None => {}
            }
            if let Some(cond) = supports {
                s.push_str(&format!(" supports({cond})"));
            }
            let m = media.serialize();
            if !m.is_empty() {
                s.push(' ');
                s.push_str(&m);
            }
            s.push(';');
            s
        }
        RuleKind::Media { media, rules } => {
            let m = media.serialize();
            if m.is_empty() {
                group("@media".to_string(), rules)
            } else {
                group(format!("@media {m}"), rules)
            }
        }
        RuleKind::Supports {
            condition, rules, ..
        } => group(format!("@supports {condition}"), rules),
        RuleKind::LayerBlock { name, rules } => {
            if name.is_empty() {
                group("@layer".to_string(), rules)
            } else {
                group(format!("@layer {name}"), rules)
            }
        }
        RuleKind::LayerStatement { names } => format!("@layer {};", names.join(", ")),
        RuleKind::Namespace { prefix, uri } => {
            if prefix.is_empty() {
                format!("@namespace {};", url_text(uri))
            } else {
                format!(
                    "@namespace {} {};",
                    serialize_identifier(prefix),
                    url_text(uri)
                )
            }
        }
        RuleKind::FontFace { style } => declarations_rule("@font-face", style),
        RuleKind::Property { name, style } => {
            declarations_rule(&format!("@property {}", serialize_identifier(name)), style)
        }
        RuleKind::Scope { start, end, rules } => {
            let mut head = String::from("@scope");
            if let Some(s) = start {
                head.push_str(&format!(" ({s})"));
            }
            if let Some(e) = end {
                head.push_str(&format!(" to ({e})"));
            }
            group(head, rules)
        }
        RuleKind::Page {
            selector,
            style,
            rules,
        } => {
            let head = if selector.is_empty() {
                "@page".to_string()
            } else {
                format!("@page {selector}")
            };
            if rules.is_empty() {
                return declarations_rule(&head, style);
            }
            let mut s = format!("{head} {{");
            let decls = style.serialize();
            if !decls.is_empty() {
                s.push(' ');
                s.push_str(&decls);
            }
            for r in rules {
                s.push(' ');
                s.push_str(&serialize_rule(&r.borrow()));
            }
            s.push_str(" }");
            s
        }
        RuleKind::Margin { name, style } => declarations_rule(&format!("@{name}"), style),
    }
}

/// The `conditionText` of a conditional rule.
pub fn condition_text(kind: &RuleKind) -> String {
    match kind {
        RuleKind::Media { media, .. } => media.serialize(),
        RuleKind::Supports { condition, .. } => condition.clone(),
        _ => String::new(),
    }
}

/// The whole text of a sheet (its rules, one per line).
pub fn serialize_sheet(sheet: &StyleSheet) -> String {
    sheet
        .rules
        .iter()
        .map(|r| serialize_rule(&r.borrow()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sheet: &SheetRef) -> Vec<String> {
        sheet
            .borrow()
            .rules
            .iter()
            .map(|r| serialize_rule(&r.borrow()))
            .collect()
    }

    #[test]
    fn style_rules_serialize() {
        let s = parse_sheet("a>b  ,  c{color:red} d { } [x=y i] {}");
        assert_eq!(
            texts(&s),
            vec!["a > b, c { color: red; }", "d { }", "[x=\"y\" i] { }"]
        );
    }

    #[test]
    fn nested_rules_serialize() {
        let s = parse_sheet("a { color: red; > b { color: blue } width: 1px }");
        assert_eq!(
            texts(&s),
            vec!["a {\n  color: red;\n  > b { color: blue; }\n  width: 1px;\n}"]
        );
    }

    #[test]
    fn import_placement() {
        let s =
            parse_sheet("a {} @import 'x.css'; @namespace svg url(http://www.w3.org/2000/svg);");
        assert_eq!(texts(&s), vec!["a { }"]);
        let s = parse_sheet("@import url(x.css) layer(base) screen; @namespace 'u';");
        assert_eq!(
            texts(&s),
            vec![
                "@import url(\"x.css\") layer(base) screen;",
                "@namespace url(\"u\");"
            ]
        );
    }

    #[test]
    fn insert_and_delete_check_hierarchy() {
        let s = parse_sheet("a {}");
        let o = Owner::Sheet(s.clone());
        assert_eq!(
            insert_rule(&o, "@import 'x';", 1),
            Err(RuleError::HierarchyRequest)
        );
        assert_eq!(insert_rule(&o, "b {}", 5), Err(RuleError::IndexSize));
        assert_eq!(insert_rule(&o, "b {", 0), Ok(0));
        assert_eq!(
            insert_rule(&o, "@namespace x 'y';", 0),
            Err(RuleError::InvalidState)
        );
        assert_eq!(insert_rule(&o, "}", 0), Err(RuleError::Syntax));
        let m = parse_sheet("@media all { a {} }");
        let media = m.borrow().rules[0].clone();
        let g = Owner::Rule(media);
        assert_eq!(
            insert_rule(&g, "@import 'x';", 0),
            Err(RuleError::HierarchyRequest)
        );
        assert_eq!(delete_rule(&g, 1), Err(RuleError::IndexSize));
        assert_eq!(delete_rule(&g, 0), Ok(()));
        assert!(g.is_empty());
    }

    #[test]
    fn supports_conditions() {
        let ns = NamespaceMap::default();
        let eval = |t: &str| supports_condition(&parser::parse_component_values(t), &ns);
        assert_eq!(eval("(color: red)"), Some(true));
        assert_eq!(eval("not (colr: red)"), Some(true));
        assert_eq!(eval("(color: red) and (unknown(1))"), Some(false));
        assert_eq!(eval("(color: red) and (x: y) or (a: b)"), None);
        assert_eq!(eval("selector(a > b)"), Some(true));
        assert_eq!(eval("selector(a >)"), Some(false));
        assert!(css_supports_condition("color: red"));
        assert!(!css_supports_property("color", "red !important"));
    }

    #[test]
    fn layers_and_registration() {
        let s = parse_sheet("@layer a, b.c; @layer { x {} } @layer initial;");
        assert_eq!(texts(&s), vec!["@layer a, b.c;", "@layer {\n  x { }\n}"]);
        assert!(syntax_valid("<length> | auto"));
        assert!(!syntax_valid("<length>+#"));
        assert_eq!(
            check_registration("<length>", Some("1em")),
            Err(RuleError::Syntax)
        );
        assert_eq!(check_registration("<length>", Some("10px")), Ok(()));
        assert_eq!(check_registration("*", None), Ok(()));
    }
}
