//! Collections (DOM §4.2.10, §4.9.1, §7.1): `NodeList`, `HTMLCollection`
//! and the `getElementsBy*` lists, `NamedNodeMap`, `DOMTokenList` and the
//! `DOMStringMap` of HTML's `dataset`.
//!
//! The live collections hold only their root and filter; every query
//! walks the current tree, so they never go stale.

use super::{Atom, Attribute, Document, DomError, DomException, Namespace, NodeId};

/// ASCII whitespace (Infra §4.6).
fn is_ascii_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{C}' | '\r' | ' ')
}

/// The "ordered set parser" (DOM §7.1): unique whitespace-separated
/// tokens in order.
pub fn ordered_set_parse(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in input.split(is_ascii_whitespace).filter(|t| !t.is_empty()) {
        if !out.iter().any(|t| t == token) {
            out.push(token.to_string());
        }
    }
    out
}

/// A namespace or local name pattern of `getElementsByTagNameNS`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameMatch<T> {
    Any,
    Exactly(T),
}

/// What a live `HTMLCollection` keeps of the descendants of its root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollectionFilter {
    /// `children`: the element children of the root.
    Children,
    /// Every descendant element.
    All,
    /// `getElementsByTagName(qualifiedName)` with a name other than `*`.
    QualifiedName(String),
    /// `getElementsByTagNameNS(namespace, localName)`.
    NamespaceAndLocal(NameMatch<Option<Namespace>>, NameMatch<String>),
    /// `getElementsByClassName(classNames)`, already parsed.
    ClassNames(Vec<String>),
    /// HTML elements with the given `name` attribute
    /// (`getElementsByName`, a `NodeList` in HTML).
    Name(String),
}

/// A live `HTMLCollection`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HtmlCollection {
    pub root: NodeId,
    pub filter: CollectionFilter,
}

impl HtmlCollection {
    /// True if `element` belongs to the collection.
    fn accepts(&self, doc: &Document, element: NodeId) -> bool {
        let Some(e) = doc.element(element) else {
            return false;
        };
        match &self.filter {
            CollectionFilter::Children => doc.parent(element) == Some(self.root),
            CollectionFilter::All => true,
            CollectionFilter::QualifiedName(name) => {
                let qualified = e.name.qualified();
                if doc.is_html() && e.name.ns == Some(Namespace::Html) {
                    qualified == name.to_ascii_lowercase()
                } else {
                    qualified == *name
                }
            }
            CollectionFilter::NamespaceAndLocal(ns, local) => {
                (match ns {
                    NameMatch::Any => true,
                    NameMatch::Exactly(n) => e.name.ns == *n,
                }) && (match local {
                    NameMatch::Any => true,
                    NameMatch::Exactly(l) => e.name.local == *l.as_str(),
                })
            }
            CollectionFilter::ClassNames(wanted) => {
                if wanted.is_empty() {
                    return false;
                }
                let classes = ordered_set_parse(e.attr("class").unwrap_or(""));
                let quirks = doc.mode == super::QuirksMode::Quirks;
                wanted.iter().all(|w| {
                    classes.iter().any(|c| {
                        if quirks {
                            c.eq_ignore_ascii_case(w)
                        } else {
                            c == w
                        }
                    })
                })
            }
            CollectionFilter::Name(name) => {
                e.name.ns == Some(Namespace::Html) && e.attr("name") == Some(name.as_str())
            }
        }
    }

    /// The elements of the collection, in tree order.
    pub fn elements(&self, doc: &Document) -> Vec<NodeId> {
        match self.filter {
            CollectionFilter::Children => doc.element_children(self.root),
            CollectionFilter::ClassNames(ref w) if w.is_empty() => Vec::new(),
            _ => doc
                .descendant_elements(self.root)
                .into_iter()
                .filter(|&e| self.accepts(doc, e))
                .collect(),
        }
    }

    /// `length`.
    pub fn length(&self, doc: &Document) -> usize {
        self.elements(doc).len()
    }

    /// `item(index)`.
    pub fn item(&self, doc: &Document, index: usize) -> Option<NodeId> {
        self.elements(doc).get(index).copied()
    }

    /// `namedItem(key)`: the first element whose ID is `key`, or an HTML
    /// element whose `name` attribute is `key`.
    pub fn named_item(&self, doc: &Document, key: &str) -> Option<NodeId> {
        if key.is_empty() {
            return None;
        }
        self.elements(doc).into_iter().find(|&n| {
            let e = doc.element(n).expect("element");
            e.attr("id") == Some(key)
                || (e.name.ns == Some(Namespace::Html) && e.attr("name") == Some(key))
        })
    }

    /// The supported property names, in order.
    pub fn supported_property_names(&self, doc: &Document) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in self.elements(doc) {
            let e = doc.element(n).expect("element");
            if let Some(id) = e.attr("id").filter(|v| !v.is_empty()) {
                if !out.iter().any(|o| o == id) {
                    out.push(id.to_string());
                }
            }
            if e.name.ns == Some(Namespace::Html) {
                if let Some(name) = e.attr("name").filter(|v| !v.is_empty()) {
                    if !out.iter().any(|o| o == name) {
                        out.push(name.to_string());
                    }
                }
            }
        }
        out
    }
}

/// A `NodeList`: live over a node's children, or a static list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeList {
    /// `childNodes`.
    Children(NodeId),
    /// A static list (`querySelectorAll`).
    Static(Vec<NodeId>),
    /// A live list of elements (`getElementsByName`).
    Live(HtmlCollection),
}

impl NodeList {
    pub fn nodes(&self, doc: &Document) -> Vec<NodeId> {
        match self {
            NodeList::Children(parent) => doc.child_nodes(*parent),
            NodeList::Static(list) => list.clone(),
            NodeList::Live(c) => c.elements(doc),
        }
    }

    pub fn length(&self, doc: &Document) -> usize {
        match self {
            NodeList::Children(parent) => doc.children(*parent).count(),
            NodeList::Static(list) => list.len(),
            NodeList::Live(c) => c.length(doc),
        }
    }

    pub fn item(&self, doc: &Document, index: usize) -> Option<NodeId> {
        match self {
            NodeList::Children(parent) => doc.child_at(*parent, index),
            NodeList::Static(list) => list.get(index).copied(),
            NodeList::Live(c) => c.item(doc, index),
        }
    }
}

impl Document {
    /// `children`.
    pub fn children_collection(&self, parent: NodeId) -> HtmlCollection {
        HtmlCollection {
            root: parent,
            filter: CollectionFilter::Children,
        }
    }

    /// `childNodes`.
    pub fn child_nodes_list(&self, parent: NodeId) -> NodeList {
        NodeList::Children(parent)
    }

    /// `getElementsByTagName(qualifiedName)` on `root`.
    pub fn get_elements_by_tag_name(&self, root: NodeId, qualified: &str) -> HtmlCollection {
        let filter = if qualified == "*" {
            CollectionFilter::All
        } else {
            CollectionFilter::QualifiedName(qualified.to_string())
        };
        HtmlCollection { root, filter }
    }

    /// `getElementsByTagNameNS(namespace, localName)` on `root`.
    pub fn get_elements_by_tag_name_ns(
        &self,
        root: NodeId,
        namespace: Option<&str>,
        local: &str,
    ) -> HtmlCollection {
        let ns = match namespace {
            Some("*") => NameMatch::Any,
            Some("") | None => NameMatch::Exactly(None),
            Some(url) => NameMatch::Exactly(Some(Namespace::from_url(url))),
        };
        let local = if local == "*" {
            NameMatch::Any
        } else {
            NameMatch::Exactly(local.to_string())
        };
        HtmlCollection {
            root,
            filter: CollectionFilter::NamespaceAndLocal(ns, local),
        }
    }

    /// `getElementsByClassName(classNames)` on `root`.
    pub fn get_elements_by_class_name(&self, root: NodeId, class_names: &str) -> HtmlCollection {
        HtmlCollection {
            root,
            filter: CollectionFilter::ClassNames(ordered_set_parse(class_names)),
        }
    }

    /// `getElementsByName(elementName)` on the document.
    pub fn get_elements_by_name(&self, name: &str) -> NodeList {
        NodeList::Live(HtmlCollection {
            root: NodeId::DOCUMENT,
            filter: CollectionFilter::Name(name.to_string()),
        })
    }
}

// ----- NamedNodeMap (DOM §4.9.1) --------------------------------------------

/// An element's `attributes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamedNodeMap {
    pub element: NodeId,
}

impl NamedNodeMap {
    pub fn length(&self, doc: &Document) -> usize {
        doc.element(self.element).map_or(0, |e| e.attrs.len())
    }

    /// `item(index)`: the `Attr` node of the attribute at `index`.
    pub fn item(&self, doc: &mut Document, index: usize) -> Option<NodeId> {
        doc.attribute_node(self.element, index)
    }

    /// `getNamedItem(qualifiedName)`.
    pub fn get_named_item(&self, doc: &mut Document, qualified: &str) -> Option<NodeId> {
        doc.get_attribute_node(self.element, qualified)
    }

    /// `getNamedItemNS(namespace, localName)`.
    pub fn get_named_item_ns(
        &self,
        doc: &mut Document,
        ns: Option<&str>,
        local: &str,
    ) -> Option<NodeId> {
        doc.get_attribute_node_ns(self.element, ns, local)
    }

    /// `setNamedItem(attr)` and `setNamedItemNS(attr)`.
    pub fn set_named_item(
        &self,
        doc: &mut Document,
        attr: NodeId,
    ) -> Result<Option<NodeId>, DomException> {
        doc.set_attribute_node(self.element, attr)
    }

    /// Removes the attribute at `index`, returning its `Attr` node.
    fn remove_at(&self, doc: &mut Document, index: usize) -> NodeId {
        let node = doc
            .attribute_node(self.element, index)
            .expect("attribute exists");
        doc.remove_attribute_at(self.element, index);
        node
    }

    /// `removeNamedItem(qualifiedName)`.
    pub fn remove_named_item(
        &self,
        doc: &mut Document,
        qualified: &str,
    ) -> Result<NodeId, DomException> {
        let index = doc
            .attribute_index_by_name(self.element, qualified)
            .ok_or(DomException::NotFoundError)?;
        Ok(self.remove_at(doc, index))
    }

    /// `removeNamedItemNS(namespace, localName)`.
    pub fn remove_named_item_ns(
        &self,
        doc: &mut Document,
        ns: Option<&str>,
        local: &str,
    ) -> Result<NodeId, DomException> {
        let ns = ns.filter(|n| !n.is_empty()).map(Namespace::from_url);
        let index = doc
            .attribute_index_ns(self.element, ns, local)
            .ok_or(DomException::NotFoundError)?;
        Ok(self.remove_at(doc, index))
    }

    /// The supported property names: qualified names, without the ones
    /// with ASCII upper alphas on an HTML element of an HTML document.
    pub fn supported_property_names(&self, doc: &Document) -> Vec<String> {
        let Some(e) = doc.element(self.element) else {
            return Vec::new();
        };
        let html = e.name.ns == Some(Namespace::Html) && doc.is_html();
        let mut out: Vec<String> = Vec::new();
        for name in e.attrs.iter().map(Attribute::qualified_name) {
            if html && name.chars().any(|c| c.is_ascii_uppercase()) {
                continue;
            }
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }
}

// ----- DOMTokenList (DOM §7.1) ------------------------------------------------

/// A `DOMTokenList` over one attribute of an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomTokenList {
    pub element: NodeId,
    pub attribute: Atom,
    /// The supported tokens, lowercase, when the attribute defines them
    /// (`relList`); `classList` has none.
    pub supported: Option<&'static [&'static str]>,
}

impl DomTokenList {
    /// `classList`.
    pub fn class_list(element: NodeId) -> DomTokenList {
        DomTokenList {
            element,
            attribute: Atom::new("class"),
            supported: None,
        }
    }

    pub fn new(
        element: NodeId,
        attribute: &str,
        supported: Option<&'static [&'static str]>,
    ) -> DomTokenList {
        DomTokenList {
            element,
            attribute: Atom::new(attribute),
            supported,
        }
    }

    fn value_of(&self, doc: &Document) -> Option<String> {
        doc.element(self.element)?
            .attr_ns(None, &self.attribute)
            .map(str::to_string)
    }

    /// The token set.
    pub fn tokens(&self, doc: &Document) -> Vec<String> {
        self.value_of(doc)
            .map(|v| ordered_set_parse(&v))
            .unwrap_or_default()
    }

    /// The update steps: write the serialized set back, unless there is no
    /// attribute and the set is empty.
    fn update(&self, doc: &mut Document, tokens: &[String]) {
        if self.value_of(doc).is_none() && tokens.is_empty() {
            return;
        }
        doc.set_attribute_value(self.element, &self.attribute, &tokens.join(" "), None, None);
    }

    fn validate(token: &str) -> Result<(), DomException> {
        if token.is_empty() {
            return Err(DomException::SyntaxError);
        }
        if token.chars().any(is_ascii_whitespace) {
            return Err(DomException::InvalidCharacterError);
        }
        Ok(())
    }

    pub fn length(&self, doc: &Document) -> usize {
        self.tokens(doc).len()
    }

    pub fn item(&self, doc: &Document, index: usize) -> Option<String> {
        self.tokens(doc).into_iter().nth(index)
    }

    pub fn contains(&self, doc: &Document, token: &str) -> bool {
        self.tokens(doc).iter().any(|t| t == token)
    }

    /// `add(...tokens)`.
    pub fn add(&self, doc: &mut Document, tokens: &[&str]) -> Result<(), DomException> {
        for t in tokens {
            Self::validate(t)?;
        }
        let mut set = self.tokens(doc);
        for t in tokens {
            if !set.iter().any(|s| s == t) {
                set.push(t.to_string());
            }
        }
        self.update(doc, &set);
        Ok(())
    }

    /// `remove(...tokens)`.
    pub fn remove(&self, doc: &mut Document, tokens: &[&str]) -> Result<(), DomException> {
        for t in tokens {
            Self::validate(t)?;
        }
        let mut set = self.tokens(doc);
        set.retain(|s| !tokens.contains(&s.as_str()));
        self.update(doc, &set);
        Ok(())
    }

    /// `toggle(token, force)`.
    pub fn toggle(
        &self,
        doc: &mut Document,
        token: &str,
        force: Option<bool>,
    ) -> Result<bool, DomException> {
        Self::validate(token)?;
        let mut set = self.tokens(doc);
        if set.iter().any(|s| s == token) {
            if force != Some(true) {
                set.retain(|s| s != token);
                self.update(doc, &set);
                return Ok(false);
            }
            return Ok(true);
        }
        if force != Some(false) {
            set.push(token.to_string());
            self.update(doc, &set);
            return Ok(true);
        }
        Ok(false)
    }

    /// `replace(token, newToken)`.
    pub fn replace(
        &self,
        doc: &mut Document,
        token: &str,
        new_token: &str,
    ) -> Result<bool, DomException> {
        if token.is_empty() || new_token.is_empty() {
            return Err(DomException::SyntaxError);
        }
        if token.chars().any(is_ascii_whitespace) || new_token.chars().any(is_ascii_whitespace) {
            return Err(DomException::InvalidCharacterError);
        }
        let set = self.tokens(doc);
        if !set.iter().any(|s| s == token) {
            return Ok(false);
        }
        // The ordered set "replace": the first of token or newToken
        // becomes newToken, the other occurrences go.
        let mut out: Vec<String> = Vec::new();
        let mut placed = false;
        for s in set {
            if s == token || s == new_token {
                if !placed {
                    out.push(new_token.to_string());
                    placed = true;
                }
            } else {
                out.push(s);
            }
        }
        self.update(doc, &out);
        Ok(true)
    }

    /// `supports(token)`.
    pub fn supports(&self, token: &str) -> Result<bool, DomError> {
        let Some(supported) = self.supported else {
            return Err(DomError::Type(format!(
                "'{}' attribute has no supported tokens.",
                self.attribute
            )));
        };
        let lowered = token.to_ascii_lowercase();
        Ok(supported.contains(&lowered.as_str()))
    }

    /// `value` getter.
    pub fn value(&self, doc: &Document) -> String {
        self.value_of(doc).unwrap_or_default()
    }

    /// `value` setter.
    pub fn set_value(&self, doc: &mut Document, value: &str) {
        doc.set_attribute_value(self.element, &self.attribute, value, None, None);
    }
}

// ----- DOMStringMap (HTML §3.2.6.6) ---------------------------------------------

/// The `DOMStringMap` of `dataset`: the `data-*` attributes of an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomStringMap {
    pub element: NodeId,
}

impl DomStringMap {
    /// The name-value pairs: each attribute without namespace whose name
    /// starts with `data-` and has no ASCII upper alpha, with the prefix
    /// removed and each `-` before an ASCII lower alpha folded into the
    /// uppercase letter.
    pub fn pairs(&self, doc: &Document) -> Vec<(String, String)> {
        let Some(e) = doc.element(self.element) else {
            return Vec::new();
        };
        e.attrs
            .iter()
            .filter(|a| a.ns.is_none())
            .filter_map(|a| {
                let rest = a.local.strip_prefix("data-")?;
                if rest.bytes().any(|b| b.is_ascii_uppercase()) {
                    return None;
                }
                let mut name = String::with_capacity(rest.len());
                let mut chars = rest.chars().peekable();
                while let Some(c) = chars.next() {
                    match chars.peek() {
                        Some(&n) if c == '-' && n.is_ascii_lowercase() => {
                            name.push(n.to_ascii_uppercase());
                            chars.next();
                        }
                        _ => name.push(c),
                    }
                }
                Some((name, a.value.clone()))
            })
            .collect()
    }

    /// The supported property names.
    pub fn supported_property_names(&self, doc: &Document) -> Vec<String> {
        self.pairs(doc).into_iter().map(|(n, _)| n).collect()
    }

    /// The value of the named property `name`.
    pub fn get(&self, doc: &Document, name: &str) -> Option<String> {
        self.pairs(doc)
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    /// The attribute name of property `name`: `data-` and `name` with a
    /// `-` before each ASCII upper alpha, lowercased.
    fn attribute_name(name: &str) -> String {
        let mut out = String::from("data-");
        for c in name.chars() {
            if c.is_ascii_uppercase() {
                out.push('-');
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The named setter: `SyntaxError` if `name` has a `-` before an
    /// ASCII lower alpha, `InvalidCharacterError` if the attribute name is
    /// not a valid attribute local name.
    pub fn set(&self, doc: &mut Document, name: &str, value: &str) -> Result<(), DomException> {
        let b = name.as_bytes();
        if b.windows(2)
            .any(|w| w[0] == b'-' && w[1].is_ascii_lowercase())
        {
            return Err(DomException::SyntaxError);
        }
        let attr = Self::attribute_name(name);
        if !super::implementation::is_valid_attribute_local_name(&attr) {
            return Err(DomException::InvalidCharacterError);
        }
        doc.set_attribute_value(self.element, &attr, value, None, None);
        Ok(())
    }

    /// The named deleter.
    pub fn delete(&self, doc: &mut Document, name: &str) {
        let attr = Self::attribute_name(name);
        doc.remove_attribute_by_name(self.element, &attr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_map_maps_data_attributes() {
        let mut doc = Document::new_html();
        let d = doc.create_html_element("div");
        doc.set_attribute(d, "data-run-type", "url");
        doc.set_attribute(d, "data-x-", "1");
        doc.set_attribute(d, "datab", "no");
        doc.set_attribute_value(d, "data-Upper", "no", None, None);
        let map = DomStringMap { element: d };
        assert_eq!(map.get(&doc, "runType").as_deref(), Some("url"));
        assert_eq!(map.supported_property_names(&doc), ["runType", "x-"]);
        assert_eq!(map.get(&doc, "Upper"), None);
        map.set(&mut doc, "fooBar", "2").unwrap();
        assert_eq!(doc.get_attribute(d, "data-foo-bar"), Some("2"));
        assert_eq!(
            map.set(&mut doc, "foo-bar", "3"),
            Err(DomException::SyntaxError)
        );
        assert_eq!(
            map.set(&mut doc, "a b", "3"),
            Err(DomException::InvalidCharacterError)
        );
        map.delete(&mut doc, "runType");
        assert_eq!(doc.get_attribute(d, "data-run-type"), None);
    }

    #[test]
    fn live_collections_follow_the_tree() {
        let mut doc = Document::new_html();
        let html = doc.create_html_element("html");
        doc.append_child(NodeId::DOCUMENT, html);
        let divs = doc.get_elements_by_tag_name(NodeId::DOCUMENT, "DIV");
        assert_eq!(divs.length(&doc), 0);
        let d = doc.create_html_element("div");
        doc.set_attribute(d, "class", "a b");
        doc.set_attribute(d, "id", "x");
        doc.append_child(html, d);
        assert_eq!(divs.length(&doc), 1);
        assert_eq!(divs.named_item(&doc, "x"), Some(d));
        let ab = doc.get_elements_by_class_name(NodeId::DOCUMENT, " b  a ");
        assert_eq!(ab.elements(&doc), vec![d]);
        let ns = doc.get_elements_by_tag_name_ns(NodeId::DOCUMENT, Some(Namespace::HTML_URL), "*");
        assert_eq!(ns.length(&doc), 2);
        let kids = doc.children_collection(html);
        assert_eq!(kids.item(&doc, 0), Some(d));
        doc.remove(d);
        assert_eq!(divs.length(&doc), 0);
    }

    #[test]
    fn token_list_operations() {
        let mut doc = Document::new_html();
        let e = doc.create_html_element("p");
        let list = DomTokenList::class_list(e);
        list.add(&mut doc, &["a", "b", "a"]).expect("add");
        assert_eq!(doc.get_attribute(e, "class"), Some("a b"));
        assert_eq!(list.add(&mut doc, &[""]), Err(DomException::SyntaxError));
        assert_eq!(
            list.add(&mut doc, &["a b"]),
            Err(DomException::InvalidCharacterError)
        );
        assert_eq!(list.toggle(&mut doc, "a", None), Ok(false));
        assert_eq!(list.replace(&mut doc, "b", "c"), Ok(true));
        assert_eq!(list.value(&doc), "c");
        assert!(list.supports("x").is_err());
        let map = NamedNodeMap { element: e };
        assert_eq!(map.length(&doc), 1);
        let attr = map.remove_named_item(&mut doc, "class").expect("removed");
        assert_eq!(doc.attr_node_value(attr).as_deref(), Some("c"));
        assert_eq!(
            map.remove_named_item(&mut doc, "class"),
            Err(DomException::NotFoundError)
        );
    }
}
