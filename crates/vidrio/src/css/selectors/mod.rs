//! Selectors Level 4: the data model, specificity, the parser
//! ([`parser`]) and matching against the DOM ([`matching`]).
//!
//! A complex selector is kept left to right as compounds joined by
//! combinators; matching walks it right to left. The cascade of K4 reuses
//! the same structures and matching functions.

pub mod matching;
pub mod parser;

use std::ops::Add;

use crate::dom::Namespace;

pub use matching::{
    DefaultStates, ElementStates, MatchContext, matches_originating, matches_selector,
    matches_selector_list, matches_slotted,
};
pub use parser::{NamespaceMap, ParseError, parse_selector_list, parse_selector_list_with};

/// A selector's specificity (Selectors 4 §16): ids, classes-like, types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Specificity {
    pub a: u32,
    pub b: u32,
    pub c: u32,
}

impl Specificity {
    pub const ZERO: Specificity = Specificity { a: 0, b: 0, c: 0 };

    pub fn new(a: u32, b: u32, c: u32) -> Specificity {
        Specificity { a, b, c }
    }
}

impl Add for Specificity {
    type Output = Specificity;

    fn add(self, o: Specificity) -> Specificity {
        Specificity {
            a: self.a.saturating_add(o.a),
            b: self.b.saturating_add(o.b),
            c: self.c.saturating_add(o.c),
        }
    }
}

/// A selector list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelectorList(pub Vec<Selector>);

impl SelectorList {
    /// The largest specificity of the list (for `:is`, `:not`, `:has`).
    pub fn max_specificity(&self) -> Specificity {
        self.0
            .iter()
            .map(|s| s.specificity)
            .max()
            .unwrap_or_default()
    }
}

/// A complex selector, or a relative one inside `:has` (whose first
/// compound is the [`Simple::Anchor`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    /// The compounds, left to right.
    pub compounds: Vec<Compound>,
    /// `combinators[i]` joins `compounds[i]` and `compounds[i + 1]`.
    pub combinators: Vec<Combinator>,
    /// The pseudo-element of the subject, if any.
    pub pseudo_element: Option<PseudoElement>,
    pub specificity: Specificity,
}

impl Selector {
    /// The subject compound.
    pub fn subject(&self) -> &Compound {
        self.compounds.last().expect("a selector has a compound")
    }
}

/// A combinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    /// Whitespace.
    Descendant,
    /// `>`.
    Child,
    /// `+`.
    NextSibling,
    /// `~`.
    SubsequentSibling,
}

/// A compound selector: simple selectors that all apply to one element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Compound(pub Vec<Simple>);

/// The namespace part of a type selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NsMatch {
    /// `*|` or no prefix without a default namespace.
    Any,
    /// `|`: no namespace.
    None,
    /// A declared namespace.
    Is(Namespace),
}

/// The value operator of an attribute selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttrOp {
    /// `=`
    Equals,
    /// `~=`
    Includes,
    /// `|=`
    DashMatch,
    /// `^=`
    Prefix,
    /// `$=`
    Suffix,
    /// `*=`
    Substring,
}

/// The case-sensitivity flag of an attribute selector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseFlag {
    /// The document language decides.
    #[default]
    Default,
    /// `i`
    Insensitive,
    /// `s`
    Sensitive,
}

/// An attribute selector.
#[derive(Clone, Debug, PartialEq)]
pub struct AttrSelector {
    /// The namespace: without prefix an attribute selector means no
    /// namespace.
    pub ns: NsMatch,
    pub local: String,
    pub local_lower: String,
    pub op: Option<(AttrOp, String)>,
    pub case: CaseFlag,
}

/// A simple selector.
#[derive(Clone, Debug, PartialEq)]
pub enum Simple {
    /// A type selector; `local_lower` is used for HTML elements in HTML
    /// documents.
    Type {
        ns: NsMatch,
        local: String,
        local_lower: String,
    },
    /// The universal selector.
    Universal(NsMatch),
    Id(String),
    Class(String),
    Attribute(Box<AttrSelector>),
    PseudoClass(Box<PseudoClass>),
    /// The anchor element of a relative selector (`:has`); never written.
    Anchor,
}

/// An `An+B` pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nth {
    pub a: i32,
    pub b: i32,
}

impl Nth {
    /// True if some n ≥ 0 gives `index` (1-based) = a·n + b.
    pub fn matches(self, index: i32) -> bool {
        if self.a == 0 {
            return index == self.b;
        }
        let diff = index - self.b;
        diff % self.a == 0 && diff / self.a >= 0
    }
}

/// Text direction for `:dir()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Ltr,
    Rtl,
}

/// The pseudo-classes whose state comes from outside the tree (user
/// interaction, form state, navigation), answered by [`ElementStates`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ElementState {
    Link,
    AnyLink,
    Visited,
    LocalLink,
    Hover,
    Active,
    Focus,
    FocusVisible,
    FocusWithin,
    Target,
    TargetWithin,
    Checked,
    Indeterminate,
    Default,
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    Valid,
    Invalid,
    InRange,
    OutOfRange,
    UserValid,
    UserInvalid,
    Autofill,
    Modal,
    PopoverOpen,
    Fullscreen,
    Open,
    Closed,
    Playing,
    Paused,
    Blank,
}

/// A pseudo-class.
#[derive(Clone, Debug, PartialEq)]
pub enum PseudoClass {
    Root,
    Empty,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    NthChild(Nth, Option<SelectorList>),
    NthLastChild(Nth, Option<SelectorList>),
    NthOfType(Nth),
    NthLastOfType(Nth),
    Is(SelectorList),
    Where(SelectorList),
    Not(SelectorList),
    /// `:has()`: relative selectors.
    Has(SelectorList),
    Scope,
    Defined,
    Host(Option<Compound>),
    HostContext(Compound),
    Lang(Vec<String>),
    Dir(Direction),
    State(ElementState),
}

/// A pseudo-element.
#[derive(Clone, Debug, PartialEq)]
pub enum PseudoElement {
    Before,
    After,
    FirstLine,
    FirstLetter,
    Marker,
    Placeholder,
    Selection,
    Backdrop,
    FileSelectorButton,
    Slotted(Compound),
    Part(Vec<String>),
}

/// The specificity of a compound selector.
pub fn compound_specificity(compound: &Compound) -> Specificity {
    let mut s = Specificity::ZERO;
    for simple in &compound.0 {
        s = s + match simple {
            Simple::Type { .. } => Specificity::new(0, 0, 1),
            Simple::Universal(_) | Simple::Anchor => Specificity::ZERO,
            Simple::Id(_) => Specificity::new(1, 0, 0),
            Simple::Class(_) | Simple::Attribute(_) => Specificity::new(0, 1, 0),
            Simple::PseudoClass(pc) => match pc.as_ref() {
                PseudoClass::Is(list) | PseudoClass::Not(list) | PseudoClass::Has(list) => {
                    list.max_specificity()
                }
                PseudoClass::Where(_) => Specificity::ZERO,
                PseudoClass::NthChild(_, Some(list)) | PseudoClass::NthLastChild(_, Some(list)) => {
                    Specificity::new(0, 1, 0) + list.max_specificity()
                }
                PseudoClass::Host(Some(c)) | PseudoClass::HostContext(c) => {
                    Specificity::new(0, 1, 0) + compound_specificity(c)
                }
                _ => Specificity::new(0, 1, 0),
            },
        };
    }
    s
}

/// The specificity of a pseudo-element.
pub fn pseudo_element_specificity(pe: &PseudoElement) -> Specificity {
    match pe {
        PseudoElement::Slotted(c) => Specificity::new(0, 0, 1) + compound_specificity(c),
        _ => Specificity::new(0, 0, 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(s: &str) -> Specificity {
        parse_selector_list(s).expect("valid").0[0].specificity
    }

    #[test]
    fn specificity_of_logical_pseudo_classes() {
        assert_eq!(spec("a"), Specificity::new(0, 0, 1));
        assert_eq!(spec("#x.y[z] a::before"), Specificity::new(1, 2, 2));
        assert_eq!(spec(":is(#a, .b) c"), Specificity::new(1, 0, 1));
        assert_eq!(spec(":where(#a) c"), Specificity::new(0, 0, 1));
        assert_eq!(spec(":not(.a, #b)"), Specificity::new(1, 0, 0));
        assert_eq!(spec(":nth-child(2n of .a, #b)"), Specificity::new(1, 1, 0));
        assert_eq!(spec(":has(> .a .b)"), Specificity::new(0, 2, 0));
        assert_eq!(spec("*"), Specificity::ZERO);
    }

    #[test]
    fn nth_patterns() {
        let odd = Nth { a: 2, b: 1 };
        assert!(odd.matches(1) && odd.matches(3) && !odd.matches(2));
        let first3 = Nth { a: -1, b: 3 };
        assert!(first3.matches(1) && first3.matches(3) && !first3.matches(4));
        assert!(Nth { a: 0, b: 2 }.matches(2));
    }
}
