//! Selector APIs (DOM §4.2.6 `ParentNode` and §4.9 `Element`): `matches`,
//! `closest`, `querySelector` and `querySelectorAll`, over the selectors
//! of [`crate::css::selectors`]. Element states that the tree does not
//! hold come from an [`ElementStates`]; the plain variants use
//! [`DefaultStates`].

use super::{Document, DomException, NodeId};
use crate::css::selectors::{
    DefaultStates, ElementStates, MatchContext, SelectorList, matches_selector_list,
    parse_selector_list,
};

/// "Parse a selector" from a selectors string; failure is a
/// `SyntaxError`.
pub fn parse_selectors(selectors: &str) -> Result<SelectorList, DomException> {
    parse_selector_list(selectors).map_err(|_| DomException::SyntaxError)
}

impl Document {
    /// `element.matches(selectors)` (also `webkitMatchesSelector`): the
    /// element is the scoping root.
    pub fn matches(&self, element: NodeId, selectors: &str) -> Result<bool, DomException> {
        self.matches_with(element, selectors, &DefaultStates)
    }

    /// [`Document::matches`] with the element states of `states`.
    pub fn matches_with(
        &self,
        element: NodeId,
        selectors: &str,
        states: &dyn ElementStates,
    ) -> Result<bool, DomException> {
        let list = parse_selectors(selectors)?;
        let ctx = MatchContext::new(states).with_scope(Some(element));
        Ok(matches_selector_list(self, &list, element, &ctx))
    }

    /// `element.closest(selectors)`: the first inclusive ancestor element
    /// that matches, with `element` as the scoping root.
    pub fn closest(
        &self,
        element: NodeId,
        selectors: &str,
    ) -> Result<Option<NodeId>, DomException> {
        self.closest_with(element, selectors, &DefaultStates)
    }

    /// [`Document::closest`] with the element states of `states`.
    pub fn closest_with(
        &self,
        element: NodeId,
        selectors: &str,
        states: &dyn ElementStates,
    ) -> Result<Option<NodeId>, DomException> {
        let list = parse_selectors(selectors)?;
        let ctx = MatchContext::new(states).with_scope(Some(element));
        let mut current = Some(element);
        while let Some(e) = current {
            if !self.is_element(e) {
                break;
            }
            if matches_selector_list(self, &list, e, &ctx) {
                return Ok(Some(e));
            }
            current = self.parent(e);
        }
        Ok(None)
    }

    /// `node.querySelector(selectors)`: the first descendant element, in
    /// tree order, that matches with `node` as the scoping root.
    pub fn query_selector(
        &self,
        node: NodeId,
        selectors: &str,
    ) -> Result<Option<NodeId>, DomException> {
        self.query_selector_with(node, selectors, &DefaultStates)
    }

    /// [`Document::query_selector`] with the element states of `states`.
    pub fn query_selector_with(
        &self,
        node: NodeId,
        selectors: &str,
        states: &dyn ElementStates,
    ) -> Result<Option<NodeId>, DomException> {
        let list = parse_selectors(selectors)?;
        let ctx = MatchContext::new(states).with_scope(Some(node));
        let mut n = self.first_child(node);
        while let Some(c) = n {
            if self.is_element(c) && matches_selector_list(self, &list, c, &ctx) {
                return Ok(Some(c));
            }
            n = self.following(c, Some(node));
        }
        Ok(None)
    }

    /// `node.querySelectorAll(selectors)`: the elements of the static
    /// `NodeList`, in tree order.
    pub fn query_selector_all(
        &self,
        node: NodeId,
        selectors: &str,
    ) -> Result<Vec<NodeId>, DomException> {
        self.query_selector_all_with(node, selectors, &DefaultStates)
    }

    /// [`Document::query_selector_all`] with the element states of
    /// `states`.
    pub fn query_selector_all_with(
        &self,
        node: NodeId,
        selectors: &str,
        states: &dyn ElementStates,
    ) -> Result<Vec<NodeId>, DomException> {
        let list = parse_selectors(selectors)?;
        let ctx = MatchContext::new(states).with_scope(Some(node));
        Ok(self
            .descendant_elements(node)
            .into_iter()
            .filter(|&e| matches_selector_list(self, &list, e, &ctx))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::parse_document;

    fn ids(doc: &Document, nodes: &[NodeId]) -> Vec<String> {
        nodes
            .iter()
            .map(|&n| doc.get_attribute(n, "id").unwrap_or("?").to_string())
            .collect()
    }

    #[test]
    fn query_matches_and_closest() {
        let doc = parse_document(
            "<div id=a class=box><p id=b><span id=c></span></p><p id=d></p></div>",
            false,
        );
        let a = doc.get_element_by_id(NodeId::DOCUMENT, "a").expect("a");
        let c = doc.get_element_by_id(NodeId::DOCUMENT, "c").expect("c");
        assert_eq!(
            ids(
                &doc,
                &doc.query_selector_all(NodeId::DOCUMENT, "p").expect("ok")
            ),
            vec!["b", "d"]
        );
        assert_eq!(
            ids(
                &doc,
                &doc.query_selector_all(a, ":scope > p:has(span)")
                    .expect("ok")
            ),
            vec!["b"]
        );
        assert_eq!(
            doc.query_selector(a, "#d"),
            Ok(doc.get_element_by_id(NodeId::DOCUMENT, "d"))
        );
        assert_eq!(doc.query_selector(a, "#a"), Ok(None));
        assert_eq!(doc.matches(c, "div span"), Ok(true));
        assert_eq!(doc.closest(c, ".box"), Ok(Some(a)));
        assert_eq!(doc.closest(c, "td"), Ok(None));
        assert_eq!(doc.matches(c, "span::before"), Ok(false));
        assert_eq!(doc.query_selector(a, "p["), Err(DomException::SyntaxError));
    }
}
