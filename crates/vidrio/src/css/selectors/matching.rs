//! Matching selectors against elements of a [`Document`] (Selectors 4
//! §17), right to left, with the shadow tree rules of CSS Scoping: in a
//! shadow tree context the host is a featureless parent of the shadow
//! root's children that only `:host`, `:host()` and `:host-context()`
//! match.

use super::{
    AttrOp, AttrSelector, CaseFlag, Combinator, Compound, Direction, ElementState, NsMatch,
    PseudoClass, PseudoElement, Selector, SelectorList, Simple,
};
use crate::dom::collections::ordered_set_parse;
use crate::dom::{CustomElementState, Document, Namespace, NodeData, NodeId, QuirksMode};

/// The states of elements that the tree does not hold (focus, hover, form
/// state); the event loop and the HTML elements of later sprints answer
/// them. [`DefaultStates`] reads what attributes can tell.
pub trait ElementStates {
    fn matches_state(&self, doc: &Document, element: NodeId, state: ElementState) -> bool;
}

/// Element states from attributes only: links, `checked`, `selected`,
/// `disabled` (with `fieldset`), `required`, `readonly`, `placeholder`,
/// `open`. Interaction states never match.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultStates;

const FORM_CONTROLS: [&str; 5] = ["button", "input", "select", "textarea", "fieldset"];

fn html_local(doc: &Document, element: NodeId) -> Option<&'static str> {
    let e = doc.element(element)?;
    (e.name.ns == Some(Namespace::Html)).then(|| e.name.local.as_str())
}

/// True if `element` is disabled (HTML §4.10.18.5 and §4.10.10).
fn is_disabled(doc: &Document, element: NodeId) -> bool {
    let Some(local) = html_local(doc, element) else {
        return false;
    };
    let has = |n: NodeId, a: &str| doc.get_attribute(n, a).is_some();
    match local {
        "option" => {
            has(element, "disabled")
                || doc
                    .parent(element)
                    .is_some_and(|p| html_local(doc, p) == Some("optgroup") && has(p, "disabled"))
        }
        "optgroup" => has(element, "disabled"),
        l if FORM_CONTROLS.contains(&l) => {
            if has(element, "disabled") {
                return true;
            }
            // A disabled fieldset ancestor, unless inside its first legend.
            let mut child = element;
            let mut current = doc.parent(element);
            while let Some(a) = current {
                if html_local(doc, a) == Some("fieldset") && has(a, "disabled") {
                    let first_legend = doc
                        .children(a)
                        .find(|&c| html_local(doc, c) == Some("legend"));
                    if first_legend != Some(child) {
                        return true;
                    }
                }
                child = a;
                current = doc.parent(a);
            }
            false
        }
        _ => false,
    }
}

fn input_type(doc: &Document, element: NodeId) -> String {
    doc.get_attribute(element, "type")
        .unwrap_or("text")
        .to_ascii_lowercase()
}

impl ElementStates for DefaultStates {
    fn matches_state(&self, doc: &Document, element: NodeId, state: ElementState) -> bool {
        let Some(local) = html_local(doc, element) else {
            return false;
        };
        let has = |a: &str| doc.get_attribute(element, a).is_some();
        let is_input = local == "input";
        match state {
            ElementState::Link | ElementState::AnyLink => {
                matches!(local, "a" | "area") && has("href")
            }
            ElementState::Checked => {
                (is_input
                    && matches!(input_type(doc, element).as_str(), "checkbox" | "radio")
                    && has("checked"))
                    || (local == "option" && has("selected"))
            }
            ElementState::Default => {
                (is_input
                    && matches!(input_type(doc, element).as_str(), "checkbox" | "radio")
                    && has("checked"))
                    || (local == "option" && has("selected"))
            }
            ElementState::Indeterminate => local == "progress" && !has("value"),
            ElementState::Disabled => is_disabled(doc, element),
            ElementState::Enabled => {
                (FORM_CONTROLS.contains(&local) || matches!(local, "option" | "optgroup"))
                    && !is_disabled(doc, element)
            }
            ElementState::Required => {
                matches!(local, "input" | "select" | "textarea") && has("required")
            }
            ElementState::Optional => {
                matches!(local, "input" | "select" | "textarea") && !has("required")
            }
            ElementState::ReadWrite | ElementState::ReadOnly => {
                let writable = match local {
                    "input" => {
                        !has("readonly")
                            && !is_disabled(doc, element)
                            && !matches!(
                                input_type(doc, element).as_str(),
                                "hidden"
                                    | "range"
                                    | "color"
                                    | "checkbox"
                                    | "radio"
                                    | "file"
                                    | "submit"
                                    | "image"
                                    | "reset"
                                    | "button"
                            )
                    }
                    "textarea" => !has("readonly") && !is_disabled(doc, element),
                    _ => doc
                        .get_attribute(element, "contenteditable")
                        .is_some_and(|v| v.is_empty() || v.eq_ignore_ascii_case("true")),
                };
                writable == (state == ElementState::ReadWrite)
            }
            ElementState::PlaceholderShown => {
                (is_input
                    && has("placeholder")
                    && doc.get_attribute(element, "value").unwrap_or("").is_empty())
                    || (local == "textarea"
                        && has("placeholder")
                        && doc.child_text_content(element).is_empty())
            }
            ElementState::Open => matches!(local, "details" | "dialog") && has("open"),
            ElementState::Closed => matches!(local, "details" | "dialog") && !has("open"),
            _ => false,
        }
    }
}

/// What matching needs besides the document and the selector.
#[derive(Clone, Copy)]
pub struct MatchContext<'a> {
    /// The `:scope` element (the scoping root), if it is an element.
    pub scope: Option<NodeId>,
    /// The shadow host when matching in the context of its shadow tree.
    pub shadow_host: Option<NodeId>,
    pub states: &'a dyn ElementStates,
    /// The anchor of the relative selector being matched (`:has`).
    anchor: Option<NodeId>,
}

impl<'a> MatchContext<'a> {
    pub fn new(states: &'a dyn ElementStates) -> MatchContext<'a> {
        MatchContext {
            scope: None,
            shadow_host: None,
            states,
            anchor: None,
        }
    }

    pub fn with_scope(mut self, scope: Option<NodeId>) -> MatchContext<'a> {
        self.scope = scope;
        self
    }

    pub fn with_shadow_host(mut self, host: Option<NodeId>) -> MatchContext<'a> {
        self.shadow_host = host;
        self
    }

    fn featureless(&self, element: NodeId) -> bool {
        self.shadow_host == Some(element)
    }
}

/// The parent element for matching: the host stands as the parent of the
/// shadow root's children in its shadow tree context.
fn parent_for_matching(doc: &Document, element: NodeId, ctx: &MatchContext) -> Option<NodeId> {
    if ctx.featureless(element) {
        return None;
    }
    let p = doc.parent(element)?;
    if doc.is_element(p) {
        return Some(p);
    }
    if ctx.shadow_host.is_some() && doc.shadow_root_host(p) == ctx.shadow_host {
        return ctx.shadow_host;
    }
    None
}

fn previous_sibling_for_matching(
    doc: &Document,
    element: NodeId,
    ctx: &MatchContext,
) -> Option<NodeId> {
    if ctx.featureless(element) {
        return None;
    }
    doc.previous_element_sibling(element)
}

/// True if any selector of `list` matches `element` (selectors with a
/// pseudo-element never match an element).
pub fn matches_selector_list(
    doc: &Document,
    list: &SelectorList,
    element: NodeId,
    ctx: &MatchContext,
) -> bool {
    list.0
        .iter()
        .any(|s| matches_selector(doc, s, element, ctx))
}

/// True if `selector` matches `element`.
pub fn matches_selector(
    doc: &Document,
    selector: &Selector,
    element: NodeId,
    ctx: &MatchContext,
) -> bool {
    if selector.pseudo_element.is_some() || !doc.is_element(element) {
        return false;
    }
    match_from(doc, selector, selector.compounds.len() - 1, element, ctx)
}

/// True if a `::slotted(…)` selector applies to `element`: the element
/// matches the argument and its assigned slot matches the rest.
pub fn matches_slotted(
    doc: &Document,
    selector: &Selector,
    element: NodeId,
    ctx: &MatchContext,
) -> bool {
    let Some(PseudoElement::Slotted(arg)) = &selector.pseudo_element else {
        return false;
    };
    let plain = MatchContext {
        shadow_host: None,
        ..*ctx
    };
    if !match_compound(doc, arg, element, &plain) {
        return false;
    }
    let Some(slot) = doc.assigned_slot_internal(element) else {
        return false;
    };
    match_from(doc, selector, selector.compounds.len() - 1, slot, ctx)
}

/// Matches compounds `0..=index` with `compounds[index]` on `element`.
fn match_from(
    doc: &Document,
    sel: &Selector,
    index: usize,
    element: NodeId,
    ctx: &MatchContext,
) -> bool {
    if !match_compound(doc, &sel.compounds[index], element, ctx) {
        return false;
    }
    if index == 0 {
        return true;
    }
    let next = index - 1;
    match sel.combinators[next] {
        Combinator::Child => parent_for_matching(doc, element, ctx)
            .is_some_and(|p| match_from(doc, sel, next, p, ctx)),
        Combinator::Descendant => {
            let mut p = parent_for_matching(doc, element, ctx);
            while let Some(a) = p {
                if match_from(doc, sel, next, a, ctx) {
                    return true;
                }
                p = parent_for_matching(doc, a, ctx);
            }
            false
        }
        Combinator::NextSibling => previous_sibling_for_matching(doc, element, ctx)
            .is_some_and(|s| match_from(doc, sel, next, s, ctx)),
        Combinator::SubsequentSibling => {
            let mut s = previous_sibling_for_matching(doc, element, ctx);
            while let Some(x) = s {
                if match_from(doc, sel, next, x, ctx) {
                    return true;
                }
                s = previous_sibling_for_matching(doc, x, ctx);
            }
            false
        }
    }
}

fn match_compound(
    doc: &Document,
    compound: &Compound,
    element: NodeId,
    ctx: &MatchContext,
) -> bool {
    let featureless = ctx.featureless(element);
    compound.0.iter().all(|simple| {
        if featureless {
            match simple {
                Simple::PseudoClass(pc) => match pc.as_ref() {
                    PseudoClass::Host(_)
                    | PseudoClass::HostContext(_)
                    | PseudoClass::Is(_)
                    | PseudoClass::Where(_)
                    | PseudoClass::Not(_) => match_pseudo_class(doc, pc, element, ctx),
                    _ => false,
                },
                Simple::Anchor => ctx.anchor == Some(element),
                _ => false,
            }
        } else {
            match_simple(doc, simple, element, ctx)
        }
    })
}

fn ns_matches(ns: NsMatch, actual: Option<Namespace>) -> bool {
    match ns {
        NsMatch::Any => true,
        NsMatch::None => actual.is_none(),
        NsMatch::Is(n) => actual == Some(n),
    }
}

fn quirks(doc: &Document) -> bool {
    doc.mode == QuirksMode::Quirks
}

fn match_simple(doc: &Document, simple: &Simple, element: NodeId, ctx: &MatchContext) -> bool {
    let Some(e) = doc.element(element) else {
        return false;
    };
    match simple {
        Simple::Type {
            ns,
            local,
            local_lower,
        } => {
            if !ns_matches(*ns, e.name.ns) {
                return false;
            }
            if e.name.ns == Some(Namespace::Html) && doc.is_html() {
                e.name.local == *local_lower.as_str()
            } else {
                e.name.local == *local.as_str()
            }
        }
        Simple::Universal(ns) => ns_matches(*ns, e.name.ns),
        Simple::Id(id) => e.attr("id").is_some_and(|v| {
            if quirks(doc) {
                v.eq_ignore_ascii_case(id)
            } else {
                v == id
            }
        }),
        Simple::Class(class) => e.attr("class").is_some_and(|v| {
            ordered_set_parse(v).iter().any(|c| {
                if quirks(doc) {
                    c.eq_ignore_ascii_case(class)
                } else {
                    c == class
                }
            })
        }),
        Simple::Attribute(a) => match_attribute(doc, a, element),
        Simple::PseudoClass(pc) => match_pseudo_class(doc, pc, element, ctx),
        Simple::Anchor => ctx.anchor == Some(element),
    }
}

/// Attribute names whose values match ASCII case-insensitively on HTML
/// elements of HTML documents (HTML §4.16.2).
const CASE_INSENSITIVE_ATTRIBUTES: [&str; 45] = [
    "accept",
    "accept-charset",
    "align",
    "alink",
    "axis",
    "bgcolor",
    "charset",
    "checked",
    "clear",
    "codetype",
    "color",
    "compact",
    "declare",
    "defer",
    "dir",
    "direction",
    "disabled",
    "enctype",
    "face",
    "frame",
    "hreflang",
    "http-equiv",
    "lang",
    "language",
    "link",
    "media",
    "method",
    "multiple",
    "nohref",
    "noresize",
    "noshade",
    "nowrap",
    "readonly",
    "rel",
    "rev",
    "rules",
    "scope",
    "scrolling",
    "selected",
    "shape",
    "target",
    "text",
    "type",
    "valign",
    "valuetype",
];

fn match_attribute(doc: &Document, sel: &AttrSelector, element: NodeId) -> bool {
    let Some(e) = doc.element(element) else {
        return false;
    };
    let html = e.name.ns == Some(Namespace::Html) && doc.is_html();
    let name = if html { &sel.local_lower } else { &sel.local };
    e.attrs
        .iter()
        .filter(|a| ns_matches(sel.ns, a.ns) && a.local == *name.as_str())
        .any(|a| {
            let Some((op, expected)) = &sel.op else {
                return true;
            };
            let insensitive = match sel.case {
                CaseFlag::Insensitive => true,
                CaseFlag::Sensitive => false,
                CaseFlag::Default => {
                    html && a.ns.is_none()
                        && CASE_INSENSITIVE_ATTRIBUTES.contains(&a.local.as_str())
                }
            };
            let (value, expected) = if insensitive {
                (a.value.to_ascii_lowercase(), expected.to_ascii_lowercase())
            } else {
                (a.value.clone(), expected.clone())
            };
            match op {
                AttrOp::Equals => value == expected,
                AttrOp::Includes => {
                    !expected.is_empty()
                        && !expected.contains([' ', '\t', '\n', '\r', '\u{C}'])
                        && value
                            .split([' ', '\t', '\n', '\r', '\u{C}'])
                            .any(|t| t == expected)
                }
                AttrOp::DashMatch => {
                    value == expected || value.starts_with(&format!("{expected}-"))
                }
                AttrOp::Prefix => !expected.is_empty() && value.starts_with(&expected),
                AttrOp::Suffix => !expected.is_empty() && value.ends_with(&expected),
                AttrOp::Substring => !expected.is_empty() && value.contains(&expected),
            }
        })
}

fn same_type(doc: &Document, a: NodeId, b: NodeId) -> bool {
    match (doc.element(a), doc.element(b)) {
        (Some(x), Some(y)) => x.name.ns == y.name.ns && x.name.local == y.name.local,
        _ => false,
    }
}

/// The 1-based position of `element` among the element siblings on one
/// side that satisfy `keep` (counting itself).
fn position(doc: &Document, element: NodeId, from_end: bool, keep: &dyn Fn(NodeId) -> bool) -> i32 {
    let mut index = 1;
    let mut s = if from_end {
        doc.next_element_sibling(element)
    } else {
        doc.previous_element_sibling(element)
    };
    while let Some(x) = s {
        if keep(x) {
            index += 1;
        }
        s = if from_end {
            doc.next_element_sibling(x)
        } else {
            doc.previous_element_sibling(x)
        };
    }
    index
}

/// The language of `element` (HTML §3.2.6.2): the nearest `xml:lang` or
/// HTML `lang` attribute.
fn language(doc: &Document, element: NodeId) -> Option<String> {
    let mut current = Some(element);
    while let Some(n) = current {
        if let Some(e) = doc.element(n) {
            if let Some(v) = e.attr_ns(Some(Namespace::Xml), "lang") {
                return Some(v.to_string());
            }
            if e.name.ns == Some(Namespace::Html) {
                if let Some(v) = e.attr("lang") {
                    return Some(v.to_string());
                }
            }
        }
        current = doc.parent(n).or_else(|| doc.shadow_root_host(n));
    }
    None
}

/// Extended filtering of a language range against a tag (RFC 4647 §3.3.2,
/// simplified to prefix matching on subtag boundaries with `*`).
fn lang_matches(range: &str, tag: &str) -> bool {
    if tag.is_empty() {
        return false;
    }
    if range == "*" {
        return true;
    }
    let range = range.to_ascii_lowercase();
    let tag = tag.to_ascii_lowercase();
    let range = range.strip_prefix("*-").unwrap_or(&range);
    tag == range || tag.starts_with(&format!("{range}-"))
}

/// The directionality of `element`, from `dir` attributes (`auto` and no
/// attribute count as the parent's, the root's being ltr).
fn direction(doc: &Document, element: NodeId) -> Direction {
    let mut current = Some(element);
    while let Some(n) = current {
        if let Some(d) = doc.get_attribute(n, "dir") {
            if d.eq_ignore_ascii_case("rtl") {
                return Direction::Rtl;
            }
            if d.eq_ignore_ascii_case("ltr") {
                return Direction::Ltr;
            }
        }
        current = doc.parent(n).filter(|&p| doc.is_element(p));
    }
    Direction::Ltr
}

fn match_pseudo_class(
    doc: &Document,
    pc: &PseudoClass,
    element: NodeId,
    ctx: &MatchContext,
) -> bool {
    let featureless = ctx.featureless(element);
    match pc {
        PseudoClass::Root => doc.parent(element) == Some(NodeId::DOCUMENT),
        PseudoClass::Empty => doc.children(element).all(|c| match doc.data(c) {
            NodeData::Element(_) => false,
            NodeData::Text(s) | NodeData::CDataSection(s) => s.is_empty(),
            _ => true,
        }),
        PseudoClass::FirstChild => doc.previous_element_sibling(element).is_none(),
        PseudoClass::LastChild => doc.next_element_sibling(element).is_none(),
        PseudoClass::OnlyChild => {
            doc.previous_element_sibling(element).is_none()
                && doc.next_element_sibling(element).is_none()
        }
        PseudoClass::FirstOfType => {
            position(doc, element, false, &|x| same_type(doc, x, element)) == 1
        }
        PseudoClass::LastOfType => {
            position(doc, element, true, &|x| same_type(doc, x, element)) == 1
        }
        PseudoClass::OnlyOfType => {
            position(doc, element, false, &|x| same_type(doc, x, element)) == 1
                && position(doc, element, true, &|x| same_type(doc, x, element)) == 1
        }
        PseudoClass::NthChild(nth, of) | PseudoClass::NthLastChild(nth, of) => {
            let from_end = matches!(pc, PseudoClass::NthLastChild(..));
            match of {
                Some(list) => {
                    if !matches_selector_list(doc, list, element, ctx) {
                        return false;
                    }
                    let keep = |x: NodeId| matches_selector_list(doc, list, x, ctx);
                    nth.matches(position(doc, element, from_end, &keep))
                }
                None => nth.matches(position(doc, element, from_end, &|_| true)),
            }
        }
        PseudoClass::NthOfType(nth) => nth.matches(position(doc, element, false, &|x| {
            same_type(doc, x, element)
        })),
        PseudoClass::NthLastOfType(nth) => nth.matches(position(doc, element, true, &|x| {
            same_type(doc, x, element)
        })),
        PseudoClass::Is(list) | PseudoClass::Where(list) => {
            matches_selector_list(doc, list, element, ctx)
        }
        PseudoClass::Not(list) => !matches_selector_list(doc, list, element, ctx),
        PseudoClass::Has(list) => match_has(doc, list, element, ctx),
        PseudoClass::Scope => match ctx.scope {
            Some(scope) if doc.is_element(scope) => element == scope,
            _ => doc.parent(element) == Some(NodeId::DOCUMENT),
        },
        PseudoClass::Defined => doc.element(element).is_some_and(|e| {
            matches!(
                e.custom_state,
                CustomElementState::Uncustomized | CustomElementState::Custom
            )
        }),
        PseudoClass::Host(arg) => {
            featureless
                && match arg {
                    None => true,
                    Some(c) => match_compound(
                        doc,
                        c,
                        element,
                        &MatchContext {
                            shadow_host: None,
                            ..*ctx
                        },
                    ),
                }
        }
        PseudoClass::HostContext(c) => {
            if !featureless {
                return false;
            }
            let plain = MatchContext {
                shadow_host: None,
                ..*ctx
            };
            let mut current = Some(element);
            while let Some(n) = current {
                if doc.is_element(n) && match_compound(doc, c, n, &plain) {
                    return true;
                }
                current = doc.parent(n).or_else(|| doc.shadow_root_host(n));
            }
            false
        }
        PseudoClass::Lang(ranges) => {
            let tag = language(doc, element).unwrap_or_default();
            ranges.iter().any(|r| lang_matches(r, &tag))
        }
        PseudoClass::Dir(d) => direction(doc, element) == *d,
        PseudoClass::State(state) => ctx.states.matches_state(doc, element, *state),
    }
}

/// `:has()`: some element related to `anchor` as a relative selector says
/// matches it. The candidates are the descendants of the anchor and its
/// following siblings with their descendants; the anchor compound pins
/// the relation.
fn match_has(doc: &Document, list: &SelectorList, anchor: NodeId, ctx: &MatchContext) -> bool {
    let inner = MatchContext {
        anchor: Some(anchor),
        ..*ctx
    };
    let mut candidates = doc.descendant_elements(anchor);
    let mut s = doc.next_element_sibling(anchor);
    while let Some(x) = s {
        candidates.push(x);
        candidates.extend(doc.descendant_elements(x));
        s = doc.next_element_sibling(x);
    }
    list.0.iter().any(|rel| {
        let sibling_relative = rel
            .combinators
            .iter()
            .any(|c| matches!(c, Combinator::NextSibling | Combinator::SubsequentSibling));
        candidates.iter().any(|&c| {
            (sibling_relative || doc.is_ancestor(anchor, c))
                && match_from(doc, rel, rel.compounds.len() - 1, c, &inner)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::selectors::parse_selector_list;
    use crate::html::parse_document;

    fn select(doc: &Document, s: &str) -> Vec<String> {
        let list = parse_selector_list(s).expect("valid");
        let ctx = MatchContext::new(&DefaultStates);
        doc.descendant_elements(NodeId::DOCUMENT)
            .into_iter()
            .filter(|&e| matches_selector_list(doc, &list, e, &ctx))
            .map(|e| doc.get_attribute(e, "id").unwrap_or("?").to_string())
            .collect()
    }

    #[test]
    fn structural_and_logical() {
        let doc = parse_document(
            "<ul id=l><li id=a class=x><li id=b><li id=c class=x><li id=d class=x></ul><p id=p lang=en-GB><span id=s></span></p>",
            false,
        );
        assert_eq!(select(&doc, "li:nth-child(2n+1)"), vec!["a", "c"]);
        assert_eq!(select(&doc, "li:nth-child(2 of .x)"), vec!["c"]);
        assert_eq!(select(&doc, "li:nth-last-child(1 of .x)"), vec!["d"]);
        assert_eq!(select(&doc, "ul:has(> .x + li)"), vec!["l"]);
        assert_eq!(select(&doc, "li:has(+ #c)"), vec!["b"]);
        assert_eq!(select(&doc, "li:has(~ #a)"), Vec::<String>::new());
        assert_eq!(select(&doc, "li:not(.x):is(#b, #z)"), vec!["b"]);
        assert_eq!(select(&doc, ":lang(en) > span"), vec!["s"]);
        assert_eq!(select(&doc, "span:empty:only-child"), vec!["s"]);
        assert_eq!(select(&doc, "[CLASS=X i]"), vec!["a", "c", "d"]);
        assert_eq!(select(&doc, "li:last-of-type"), vec!["d"]);
    }
}
