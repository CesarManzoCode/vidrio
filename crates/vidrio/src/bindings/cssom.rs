//! The CSSOM members over [`crate::css::om`] and [`crate::css::sheets`]:
//! `StyleSheetList`, `CSSStyleSheet` (constructible, `replace`,
//! `replaceSync`, `insertRule`/`deleteRule`, `addRule`/`removeRule`),
//! `adoptedStyleSheets`, `LinkStyle.sheet`, `CSSRuleList`, every
//! `CSSRule` subclass, the `CSSStyleDeclaration` of rules (with the
//! named property attributes of `CSSStyleProperties` and of the
//! descriptor interfaces), `MediaList`, the `CSS` namespace and
//! `matchMedia` with its `MediaQueryList`, and the read-only
//! `CSSStyleProperties` of `getComputedStyle` over [`crate::css::style`].
//!
//! One object stands for each sheet, rule and rule list: the realm's
//! identity map ([`super::RealmState::cssom`]) keeps it.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

use vjs::{Agent, JsResult, Object, Throw, Value};

use super::RealmState;
use super::exception;
use super::generated::{Op, interface_by_name};
use super::platform::{self, Native};
use super::value::{IdlValue, Invocation};
use crate::css::cascade::{self, Pseudo};
use crate::css::declaration::DeclarationBlock;
use crate::css::media::{self, Environment, MediaQueryList};
use crate::css::om::{self, Owner, RuleError, RuleKind, RuleRef, SheetRef};
use crate::css::properties;
use crate::css::sheets;
use crate::css::style;
use crate::css::values::custom::Registration;
use crate::dom::{DocRef, NodeId};

type R = JsResult<IdlValue>;

/// A `CSSStyleSheet`.
pub struct SheetData(pub SheetRef);

/// A `StyleSheetList` of a document or shadow root.
pub struct SheetListData {
    pub doc: DocRef,
    pub root: NodeId,
}

/// A `CSSRuleList`.
pub struct RuleListData(pub Owner);

/// A `CSSRule`.
pub struct RuleData(pub RuleRef);

/// The `CSSStyleDeclaration` of a rule.
pub struct DeclData(pub RuleRef);

/// What a `MediaList` belongs to.
pub enum MediaOwner {
    Sheet(SheetRef),
    Rule(RuleRef),
}

/// A `MediaList`.
pub struct MediaData(pub MediaOwner);

/// The `CSSStyleProperties` of `getComputedStyle`: live, it resolves
/// the element's style on every access.
pub struct ComputedData {
    pub doc: DocRef,
    pub element: NodeId,
    pub pseudo: Option<Pseudo>,
    /// False for an unknown pseudo-element: every value is empty.
    pub valid: bool,
}

/// A `MediaQueryList`.
pub struct MqlData {
    pub list: MediaQueryList,
}

thread_local! {
    /// The registered custom properties, by realm address.
    static REGISTERED: RefCell<HashSet<(usize, String)>> = RefCell::new(HashSet::new());
}

/// The operations that may change a sheet, a rule or a declaration.
const MUTATORS: &[&str] = &[
    "insertRule",
    "deleteRule",
    "addRule",
    "removeRule",
    "replace",
    "replaceSync",
    "setProperty",
    "removeProperty",
    "appendMedium",
    "deleteMedium",
    "registerProperty",
];

/// Runs the CSSOM member `inv.op`, or returns `None` if it is not one.
pub fn call(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> Option<R> {
    let name = inv.op.name();
    let (owner, member) = name.split_once('_')?;
    if member.starts_with("get_on") || member.starts_with("set_on") {
        return None;
    }
    if member.starts_with("set_") || MUTATORS.contains(&member) {
        om::sheets_changed();
    }
    Some(match owner {
        "StyleSheet" | "CSSStyleSheet" => sheet(agent, state, inv, member),
        "StyleSheetList" => sheet_list(agent, state, inv, member),
        "CSSRuleList" => rule_list(agent, state, inv, member),
        "CSSRule"
        | "CSSStyleRule"
        | "CSSImportRule"
        | "CSSGroupingRule"
        | "CSSPageRule"
        | "CSSMarginRule"
        | "CSSNamespaceRule"
        | "CSSConditionRule"
        | "CSSMediaRule"
        | "CSSSupportsRule"
        | "CSSLayerBlockRule"
        | "CSSLayerStatementRule"
        | "CSSScopeRule"
        | "CSSPropertyRule"
        | "CSSNestedDeclarations"
        | "CSSFontFaceRule" => rule(agent, state, inv, member),
        "CSSStyleDeclaration" | "CSSStyleProperties"
            if inv
                .this
                .as_ref()
                .and_then(crate::fetch::data_of::<ComputedData>)
                .is_some() =>
        {
            computed(agent, inv, member)
        }
        "CSSStyleDeclaration"
        | "CSSStyleProperties"
        | "CSSFontFaceDescriptors"
        | "CSSPageDescriptors" => {
            let d = inv
                .this
                .as_ref()
                .and_then(crate::fetch::data_of::<DeclData>)?;
            declaration(agent, state, inv, member, &d.0)
        }
        "MediaList" => media_list(agent, inv, member),
        "DocumentOrShadowRoot" => match member {
            "get_styleSheets" => {
                let (doc, root) = this_node(inv)?;
                Ok(new_data(
                    agent,
                    state,
                    "StyleSheetList",
                    Rc::new(SheetListData { doc, root }),
                ))
            }
            "get_adoptedStyleSheets" => {
                let (doc, root) = this_node(inv)?;
                let list = sheets::adopted(&doc, root);
                Ok(IdlValue::Sequence(
                    list.iter()
                        .map(|s| IdlValue::Object(wrap_sheet(agent, state, s)))
                        .collect(),
                ))
            }
            "set_adoptedStyleSheets" => {
                let (doc, root) = this_node(inv)?;
                set_adopted(agent, inv, &doc, root)
            }
            _ => return None,
        },
        "LinkStyle" if member == "get_sheet" => {
            let (doc, node) = this_node(inv)?;
            Ok(match sheets::sheet_of(&doc, node) {
                Some(s) => IdlValue::Object(wrap_sheet(agent, state, &s)),
                None => IdlValue::Null,
            })
        }
        "CSS" => css_namespace(agent, state, inv, member),
        "Window" if member == "matchMedia" => match_media(agent, state, inv),
        "Window" if member == "getComputedStyle" => get_computed_style(agent, state, inv),
        "MediaQueryList" => media_query_list(agent, state, inv, member),
        _ => return None,
    })
}

// ----- helpers ---------------------------------------------------------------------

fn throw<T>(agent: &mut Agent, e: RuleError) -> JsResult<T> {
    let o = exception::create(agent, e.name(), "");
    Err(Throw(Value::Object(o)))
}

fn string(s: impl Into<String>) -> IdlValue {
    IdlValue::String(s.into())
}

fn this_node(inv: &Invocation) -> Option<(DocRef, NodeId)> {
    let p = inv.this.as_ref()?;
    let native = p.get().native.borrow();
    match &*native {
        Native::Node(d, n) => Some((d.clone(), *n)),
        _ => None,
    }
}

fn this<T: 'static>(agent: &mut Agent, inv: &Invocation) -> JsResult<Rc<T>> {
    crate::fetch::this_data::<T>(agent, inv)
}

fn new_data(agent: &mut Agent, state: &RealmState, iface: &str, data: Rc<dyn Any>) -> IdlValue {
    IdlValue::Object(crate::fetch::create_object(
        agent,
        state,
        iface,
        Native::Data(data),
        None,
    ))
}

/// The object of `key` in the identity map, made by `make` the first time.
fn cached(
    agent: &mut Agent,
    state: &RealmState,
    key: (usize, u8),
    keep: Rc<dyn Any>,
    make: impl FnOnce(&mut Agent) -> Object,
) -> Object {
    if let Some((_, o)) = state.cssom.borrow().get(&key) {
        return *o;
    }
    let o = make(agent);
    state.cssom.borrow_mut().insert(key, (keep, o));
    o
}

fn sheet_key(s: &SheetRef) -> (usize, u8) {
    (Rc::as_ptr(s) as *const u8 as usize, b'S')
}

/// The `CSSStyleSheet` object of `s`.
pub fn wrap_sheet(agent: &mut Agent, state: &RealmState, s: &SheetRef) -> Object {
    let keep: Rc<dyn Any> = s.clone();
    let data: Rc<dyn Any> = Rc::new(SheetData(s.clone()));
    cached(agent, state, sheet_key(s), keep, |agent| {
        crate::fetch::create_object(agent, state, "CSSStyleSheet", Native::Data(data), None)
    })
}

/// The `CSSRule` object of `r`.
pub fn wrap_rule(agent: &mut Agent, state: &RealmState, r: &RuleRef) -> Object {
    let keep: Rc<dyn Any> = r.clone();
    let iface = r.borrow().kind.interface();
    let data: Rc<dyn Any> = Rc::new(RuleData(r.clone()));
    let key = (Rc::as_ptr(r) as *const u8 as usize, b'R');
    cached(agent, state, key, keep, |agent| {
        crate::fetch::create_object(agent, state, iface, Native::Data(data), None)
    })
}

/// The `CSSRuleList` object of `owner`.
fn wrap_rule_list(agent: &mut Agent, state: &RealmState, owner: Owner) -> Object {
    let (key, keep): ((usize, u8), Rc<dyn Any>) = match &owner {
        Owner::Sheet(s) => ((Rc::as_ptr(s) as *const u8 as usize, b'L'), s.clone()),
        Owner::Rule(r) => ((Rc::as_ptr(r) as *const u8 as usize, b'L'), r.clone()),
    };
    let data: Rc<dyn Any> = Rc::new(RuleListData(owner));
    cached(agent, state, key, keep, |agent| {
        crate::fetch::create_object(agent, state, "CSSRuleList", Native::Data(data), None)
    })
}

fn opt_sheet(agent: &mut Agent, state: &RealmState, s: Option<SheetRef>) -> IdlValue {
    match s {
        Some(s) => IdlValue::Object(wrap_sheet(agent, state, &s)),
        None => IdlValue::Null,
    }
}

fn opt_rule(agent: &mut Agent, state: &RealmState, r: Option<RuleRef>) -> IdlValue {
    match r {
        Some(r) => IdlValue::Object(wrap_rule(agent, state, &r)),
        None => IdlValue::Null,
    }
}

fn not_implemented<T>(agent: &mut Agent, op: Op) -> JsResult<T> {
    agent.type_error(&format!("{} is not implemented.", op.name()))
}

// ----- style sheets ----------------------------------------------------------------

fn sheet(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    if member == "constructor" {
        return construct_sheet(agent, state, inv);
    }
    let s = this::<SheetData>(agent, inv)?.0.clone();
    Ok(match member {
        "get_type" => string("text/css"),
        "get_href" => IdlValue::opt_string(s.borrow().href.clone()),
        "get_ownerNode" => {
            let node = state
                .document()
                .and_then(|d| sheets::owner_node(&d, &s).map(|n| (d, n)));
            match node {
                Some((d, n)) => IdlValue::Object(super::wrappers::wrap_node(agent, &d, n)),
                None => IdlValue::Null,
            }
        }
        "get_parentStyleSheet" => {
            let p = s.borrow().parent_sheet.as_ref().and_then(Weak::upgrade);
            opt_sheet(agent, state, p)
        }
        "get_title" => IdlValue::opt_string(s.borrow().title.clone()),
        "get_media" => new_data(
            agent,
            state,
            "MediaList",
            Rc::new(MediaData(MediaOwner::Sheet(s.clone()))),
        ),
        "get_disabled" => IdlValue::Bool(s.borrow().disabled),
        "set_disabled" => {
            s.borrow_mut().disabled = inv.bool(0);
            IdlValue::Undefined
        }
        "get_ownerRule" => {
            let r = s.borrow().owner_rule.as_ref().and_then(Weak::upgrade);
            opt_rule(agent, state, r)
        }
        "get_cssRules" | "get_rules" => {
            if !s.borrow().origin_clean {
                return throw(agent, RuleError::Security);
            }
            IdlValue::Object(wrap_rule_list(agent, state, Owner::Sheet(s.clone())))
        }
        "insertRule" => {
            let index = match inv.arg(1) {
                IdlValue::Missing => 0,
                v => v.as_usize(),
            };
            match om::insert_rule(&Owner::Sheet(s.clone()), &inv.str(0), index) {
                Ok(i) => IdlValue::UInt(i as u64),
                Err(e) => return throw(agent, e),
            }
        }
        "deleteRule" | "removeRule" => {
            let index = match inv.arg(0) {
                IdlValue::Missing => 0,
                v => v.as_usize(),
            };
            if let Err(e) = om::delete_rule(&Owner::Sheet(s.clone()), index) {
                return throw(agent, e);
            }
            IdlValue::Undefined
        }
        "addRule" => {
            let selector = match inv.arg(0) {
                IdlValue::Missing => "undefined".to_string(),
                v => v.as_str().to_string(),
            };
            let style = match inv.arg(1) {
                IdlValue::Missing => "undefined".to_string(),
                v => v.as_str().to_string(),
            };
            let text = if style.is_empty() {
                format!("{selector} {{ }}")
            } else {
                format!("{selector} {{ {style} }}")
            };
            let owner = Owner::Sheet(s.clone());
            let index = match inv.arg(2) {
                IdlValue::Missing => owner.len(),
                v => v.as_usize(),
            };
            if let Err(e) = om::insert_rule(&owner, &text, index) {
                return throw(agent, e);
            }
            IdlValue::Int(-1)
        }
        "replaceSync" => {
            if let Err(e) = om::replace_sync(&s, &inv.str(0)) {
                return throw(agent, e);
            }
            IdlValue::Undefined
        }
        "replace" => return replace(agent, state, &s, inv.str(0)),
        _ => return not_implemented(agent, inv.op),
    })
}

/// `new CSSStyleSheet(options)`.
fn construct_sheet(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let doc_base = state
        .document()
        .map(|d| crate::browser::document::base_url(&d));
    let (base, media_text, disabled) = match inv.arg(0).as_dict() {
        Some(d) => {
            let base = d.str("baseURL").map(str::to_string);
            let media_text = match d.get("media") {
                Some(IdlValue::Object(o)) => {
                    match crate::fetch::object_data::<MediaData>(agent, *o) {
                        Some(m) => media_of(&m.0).serialize(),
                        None => String::new(),
                    }
                }
                Some(v) => v.as_str().to_string(),
                None => String::new(),
            };
            (base, media_text, d.bool("disabled").unwrap_or(false))
        }
        None => (None, String::new(), false),
    };
    let base_url = match base {
        Some(b) => {
            let parsed = crate::url::Url::parse(&b, doc_base.as_ref());
            match parsed {
                Ok(u) => Some(u.href()),
                Err(_) => return throw(agent, RuleError::NotAllowed),
            }
        }
        None => doc_base.map(|u| u.href()),
    };
    let sheet = om::constructed_sheet(base_url, &media_text, disabled);
    let proto = super::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
    let id = interface_by_name("CSSStyleSheet").unwrap_or(inv.iface);
    let data: Rc<dyn Any> = Rc::new(SheetData(sheet.clone()));
    let o = platform::create(agent, state, id, Native::Data(data), proto);
    let keep: Rc<dyn Any> = sheet.clone();
    state
        .cssom
        .borrow_mut()
        .insert(sheet_key(&sheet), (keep, o));
    Ok(IdlValue::Object(o))
}

/// `replace(text)`: the rules change in a task, then the promise
/// resolves with the sheet.
fn replace(agent: &mut Agent, state: &Rc<RealmState>, s: &SheetRef, text: String) -> R {
    let realm = state.realm;
    {
        let sh = s.borrow();
        if !sh.constructed || sh.disallow_modification {
            let e = crate::fetch::dom_exception_value(agent, realm, "NotAllowedError", "");
            return Ok(IdlValue::Object(crate::fetch::rejected(agent, realm, e)));
        }
    }
    s.borrow_mut().disallow_modification = true;
    let deferred = crate::fetch::Deferred::new(agent, realm);
    let promise = deferred.promise;
    let sheet_object = wrap_sheet(agent, state, s);
    let mut values = deferred.values();
    values.push(Value::Object(sheet_object));
    let kept = crate::fetch::keep(values);
    let sheet = s.clone();
    let doc = state.document();
    crate::browser::event_loop::queue_task(
        crate::browser::event_loop::TaskSource::DomManipulation,
        doc,
        move |agent| {
            let _kept = kept;
            om::replace_rules(&sheet, &text, false);
            sheet.borrow_mut().disallow_modification = false;
            deferred.resolve(agent, Value::Object(sheet_object));
        },
    );
    Ok(IdlValue::Object(promise))
}

fn set_adopted(agent: &mut Agent, inv: &Invocation, doc: &DocRef, root: NodeId) -> R {
    let mut list = Vec::new();
    for v in inv.arg(0).as_seq() {
        let sheet = v
            .as_object()
            .and_then(|o| crate::fetch::object_data::<SheetData>(agent, o));
        let Some(sheet) = sheet else {
            return agent.type_error("Not a CSSStyleSheet.");
        };
        if !sheet.0.borrow().constructed {
            return throw(agent, RuleError::NotAllowed);
        }
        list.push(sheet.0.clone());
    }
    sheets::set_adopted(doc, root, list);
    Ok(IdlValue::Undefined)
}

fn sheet_list(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    let l = this::<SheetListData>(agent, inv)?;
    let list = sheets::style_sheets(&l.doc, l.root);
    Ok(match member {
        "get_length" | "index_count" => IdlValue::UInt(list.len() as u64),
        "item" => match list.get(inv.usize(0)) {
            Some((_, s)) => IdlValue::Object(wrap_sheet(agent, state, s)),
            None => IdlValue::Null,
        },
        _ => return not_implemented(agent, inv.op),
    })
}

fn rule_list(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    let l = this::<RuleListData>(agent, inv)?;
    Ok(match member {
        "get_length" | "index_count" => IdlValue::UInt(l.0.len() as u64),
        "item" => {
            let r = l.0.item(inv.usize(0));
            opt_rule(agent, state, r)
        }
        _ => return not_implemented(agent, inv.op),
    })
}

// ----- rules -----------------------------------------------------------------------

fn rule(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    let r = this::<RuleData>(agent, inv)?.0.clone();
    let value = {
        let b = r.borrow();
        match (member, &b.kind) {
            ("get_cssText", _) => Some(string(om::serialize_rule(&b))),
            ("get_type", k) => Some(IdlValue::UInt(k.type_number() as u64)),
            ("get_selectorText", RuleKind::Style { selector, .. })
            | ("get_selectorText", RuleKind::Page { selector, .. }) => {
                Some(string(selector.clone()))
            }
            ("get_href", RuleKind::Import { href, .. }) => Some(string(href.clone())),
            ("get_layerName", RuleKind::Import { layer, .. }) => {
                Some(IdlValue::opt_string(layer.clone()))
            }
            ("get_supportsText", RuleKind::Import { supports, .. }) => {
                Some(IdlValue::opt_string(supports.clone()))
            }
            ("get_conditionText", k) => Some(string(om::condition_text(k))),
            ("get_matches", RuleKind::Media { media, .. }) => {
                Some(IdlValue::Bool(media.matches(&Environment::default())))
            }
            ("get_matches", RuleKind::Supports { matches, .. }) => Some(IdlValue::Bool(*matches)),
            ("get_name", RuleKind::LayerBlock { name, .. })
            | ("get_name", RuleKind::Property { name, .. })
            | ("get_name", RuleKind::Margin { name, .. }) => Some(string(name.clone())),
            ("get_nameList", RuleKind::LayerStatement { names }) => Some(IdlValue::Sequence(
                names.iter().map(|n| string(n.clone())).collect(),
            )),
            ("get_start", RuleKind::Scope { start, .. }) => {
                Some(IdlValue::opt_string(start.clone()))
            }
            ("get_end", RuleKind::Scope { end, .. }) => Some(IdlValue::opt_string(end.clone())),
            ("get_syntax", RuleKind::Property { style, .. }) => {
                Some(string(om::property_syntax(style).unwrap_or_default()))
            }
            ("get_inherits", RuleKind::Property { style, .. }) => Some(IdlValue::Bool(
                om::property_inherits(style).unwrap_or(false),
            )),
            ("get_initialValue", RuleKind::Property { style, .. }) => {
                let v = style.property_value("initial-value");
                Some(IdlValue::opt_string((!v.is_empty()).then_some(v)))
            }
            ("get_namespaceURI", RuleKind::Namespace { uri, .. }) => Some(string(uri.clone())),
            ("get_prefix", RuleKind::Namespace { prefix, .. }) => Some(string(prefix.clone())),
            _ => None,
        }
    };
    if let Some(v) = value {
        return Ok(v);
    }
    Ok(match member {
        // Setting `cssText` of a rule does nothing (CSSOM §6.4.2).
        "set_cssText" => IdlValue::Undefined,
        "get_parentRule" => {
            let p = r.borrow().parent_rule.as_ref().and_then(Weak::upgrade);
            opt_rule(agent, state, p)
        }
        "get_parentStyleSheet" => {
            let p = r.borrow().parent_sheet.as_ref().and_then(Weak::upgrade);
            opt_sheet(agent, state, p)
        }
        "set_selectorText" => {
            om::set_selector_text(&r, &inv.str(0));
            IdlValue::Undefined
        }
        "get_style" => new_data(
            agent,
            state,
            match r.borrow().kind {
                RuleKind::FontFace { .. } => "CSSFontFaceDescriptors",
                RuleKind::Page { .. } => "CSSPageDescriptors",
                RuleKind::Margin { .. } => "CSSStyleDeclaration",
                _ => "CSSStyleProperties",
            },
            Rc::new(DeclData(r.clone())),
        ),
        "get_media" => new_data(
            agent,
            state,
            "MediaList",
            Rc::new(MediaData(MediaOwner::Rule(r.clone()))),
        ),
        "get_styleSheet" => {
            let s = match &r.borrow().kind {
                RuleKind::Import { sheet, .. } => sheet.clone(),
                _ => None,
            };
            opt_sheet(agent, state, s)
        }
        "get_cssRules" => IdlValue::Object(wrap_rule_list(agent, state, Owner::Rule(r.clone()))),
        "insertRule" => {
            let index = match inv.arg(1) {
                IdlValue::Missing => 0,
                v => v.as_usize(),
            };
            match om::insert_rule(&Owner::Rule(r.clone()), &inv.str(0), index) {
                Ok(i) => IdlValue::UInt(i as u64),
                Err(e) => return throw(agent, e),
            }
        }
        "deleteRule" => {
            if let Err(e) = om::delete_rule(&Owner::Rule(r.clone()), inv.usize(0)) {
                return throw(agent, e);
            }
            IdlValue::Undefined
        }
        _ => return not_implemented(agent, inv.op),
    })
}

// ----- declarations ----------------------------------------------------------------

fn declaration(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
    r: &RuleRef,
) -> R {
    let mut b = r.borrow_mut();
    let Some(block) = b.kind.style_mut() else {
        drop(b);
        return not_implemented(agent, inv.op);
    };
    Ok(match member {
        "get_cssText" => string(block.serialize()),
        "set_cssText" => {
            *block = DeclarationBlock::parse_in(block.context, &inv.str(0));
            IdlValue::Undefined
        }
        "get_length" | "index_count" => IdlValue::UInt(block.len() as u64),
        "item" => string(block.item(inv.usize(0)).unwrap_or_default().to_string()),
        "getPropertyValue" => string(block.property_value(&inv.str(0))),
        "getPropertyPriority" => string(block.property_priority(&inv.str(0)).to_string()),
        "setProperty" => {
            block.set_property(&inv.str(0), &inv.str(1), &inv.str(2));
            IdlValue::Undefined
        }
        "removeProperty" => string(block.remove_property(&inv.str(0)).unwrap_or_default()),
        "get_parentRule" => {
            drop(b);
            IdlValue::Object(wrap_rule(agent, state, r))
        }
        m => {
            if let Some(attr) = m.strip_prefix("get_") {
                string(block.property_value(&properties::property_of_attribute(attr)))
            } else if let Some(attr) = m.strip_prefix("set_") {
                block.set_property(&properties::property_of_attribute(attr), &inv.str(0), "");
                IdlValue::Undefined
            } else {
                drop(b);
                return not_implemented(agent, inv.op);
            }
        }
    })
}

// ----- media lists -----------------------------------------------------------------

fn media_of(owner: &MediaOwner) -> MediaQueryList {
    match owner {
        MediaOwner::Sheet(s) => s.borrow().media.clone(),
        MediaOwner::Rule(r) => match &r.borrow().kind {
            RuleKind::Import { media, .. } | RuleKind::Media { media, .. } => media.clone(),
            _ => MediaQueryList::default(),
        },
    }
}

fn set_media(owner: &MediaOwner, list: MediaQueryList) {
    match owner {
        MediaOwner::Sheet(s) => s.borrow_mut().media = list,
        MediaOwner::Rule(r) => match &mut r.borrow_mut().kind {
            RuleKind::Import { media, .. } | RuleKind::Media { media, .. } => *media = list,
            _ => {}
        },
    }
}

fn media_list(agent: &mut Agent, inv: &Invocation, member: &str) -> R {
    let m = this::<MediaData>(agent, inv)?;
    let mut list = media_of(&m.0);
    Ok(match member {
        "get_mediaText" | "stringifier" => string(list.serialize()),
        "set_mediaText" => {
            set_media(&m.0, media::parse(&inv.str(0)));
            IdlValue::Undefined
        }
        "get_length" | "index_count" => IdlValue::UInt(list.queries.len() as u64),
        "item" => match list.queries.get(inv.usize(0)) {
            Some(q) => string(q.serialize()),
            None => IdlValue::Null,
        },
        "appendMedium" => {
            list.append_medium(&inv.str(0));
            set_media(&m.0, list);
            IdlValue::Undefined
        }
        "deleteMedium" => {
            if !list.delete_medium(&inv.str(0)) {
                let o = exception::create(agent, "NotFoundError", "");
                return Err(Throw(Value::Object(o)));
            }
            set_media(&m.0, list);
            IdlValue::Undefined
        }
        _ => return not_implemented(agent, inv.op),
    })
}

// ----- the CSS namespace -------------------------------------------------------------

fn css_namespace(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    Ok(match member {
        "escape" => string(om::css_escape(&inv.str(0))),
        "supports" => {
            let two = !matches!(inv.arg(1), IdlValue::Missing);
            IdlValue::Bool(if two {
                om::css_supports_property(&inv.str(0), &inv.str(1))
            } else {
                om::css_supports_condition(&inv.str(0))
            })
        }
        "registerProperty" => {
            let Some(d) = inv.arg(0).as_dict() else {
                return agent.type_error("PropertyDefinition is required.");
            };
            let name = d.str("name").unwrap_or_default().to_string();
            let syntax = d.str("syntax").unwrap_or("*").to_string();
            let initial = d.str("initialValue").map(str::to_string);
            if !crate::css::declaration::is_custom_property(&name) || name.len() < 3 {
                return throw(agent, RuleError::Syntax);
            }
            if let Err(e) = om::check_registration(&syntax, initial.as_deref()) {
                return throw(agent, e);
            }
            let inherits = d.bool("inherits").unwrap_or(false);
            let registration = Registration::new(&name, &syntax, inherits, initial.as_deref());
            let key = (Rc::as_ptr(state) as usize, name);
            let fresh = REGISTERED.with(|r| r.borrow_mut().insert(key));
            if !fresh {
                return throw(agent, RuleError::InvalidModification);
            }
            if let (Some(doc), Ok(r)) = (state.document(), registration) {
                cascade::register_property(&doc, r);
            }
            IdlValue::Undefined
        }
        _ => return not_implemented(agent, inv.op),
    })
}

// ----- getComputedStyle ------------------------------------------------------------

/// The element argument `i`, if it is a node.
fn node_arg(agent: &Agent, inv: &Invocation, i: usize) -> Option<(DocRef, NodeId)> {
    let o = inv.object(i)?;
    let p = platform::platform_ref(agent, Value::Object(o))?;
    let native = p.get().native.borrow();
    let out = match &*native {
        Native::Node(d, n) => Some((d.clone(), *n)),
        _ => None,
    };
    out
}

/// `getComputedStyle(elt, pseudoElt)` (CSSOM §9).
fn get_computed_style(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let Some((doc, element)) = node_arg(agent, inv, 0) else {
        return agent.type_error("Argument 1 is not an Element.");
    };
    if !doc.borrow().is_element(element) {
        return agent.type_error("Argument 1 is not an Element.");
    }
    let pseudo_text = match inv.arg(1) {
        IdlValue::Missing | IdlValue::Undefined | IdlValue::Null => String::new(),
        _ => inv.str(1),
    };
    // A pseudo-element argument counts only if it starts with a colon.
    let (pseudo, valid) = if pseudo_text.starts_with(':') {
        match Pseudo::parse(&pseudo_text) {
            Some(p) => (Some(p), true),
            None => (None, false),
        }
    } else {
        (None, true)
    };
    Ok(new_data(
        agent,
        state,
        "CSSStyleProperties",
        Rc::new(ComputedData {
            doc,
            element,
            pseudo,
            valid,
        }),
    ))
}

fn no_modification<T>(agent: &mut Agent) -> JsResult<T> {
    let o = exception::create(agent, "NoModificationAllowedError", "");
    Err(Throw(Value::Object(o)))
}

/// The members of a computed style declaration: the resolved values,
/// read-only.
fn computed(agent: &mut Agent, inv: &Invocation, member: &str) -> R {
    let c = this::<ComputedData>(agent, inv)?;
    let style = if !c.valid {
        None
    } else {
        match c.pseudo {
            None => style::computed_style(&c.doc, c.element),
            Some(p) => style::pseudo_style(&c.doc, c.element, p),
        }
    };
    let value = |name: &str| {
        style
            .as_ref()
            .map(|s| style::resolved_value(s, name))
            .unwrap_or_default()
    };
    let names = || {
        style
            .as_ref()
            .map(|s| style::resolved_names(s))
            .unwrap_or_default()
    };
    Ok(match member {
        "get_cssText" => string(""),
        "get_length" | "index_count" => IdlValue::UInt(names().len() as u64),
        "item" => string(names().get(inv.usize(0)).cloned().unwrap_or_default()),
        "getPropertyValue" => string(value(&inv.str(0))),
        "getPropertyPriority" => string(""),
        "get_parentRule" => IdlValue::Null,
        "get_cssFloat" => string(value("float")),
        "set_cssText" | "setProperty" | "removeProperty" => return no_modification(agent),
        m if m.starts_with("set_") => return no_modification(agent),
        m => match m.strip_prefix("get_") {
            Some(attr) => string(value(&properties::property_of_attribute(attr))),
            None => return not_implemented(agent, inv.op),
        },
    })
}

// ----- matchMedia ----------------------------------------------------------------------

fn match_media(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let list = media::parse(&inv.str(0));
    let target = crate::fetch::new_target_id(state);
    let data: Rc<dyn Any> = Rc::new(MqlData { list });
    let o = crate::fetch::create_object(
        agent,
        state,
        "MediaQueryList",
        Native::TargetData(target, data),
        None,
    );
    state.targets.borrow_mut().insert(target, o);
    Ok(IdlValue::Object(o))
}

fn media_query_list(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> R {
    let _ = state;
    let m = this::<MqlData>(agent, inv)?;
    Ok(match member {
        "get_media" => string(m.list.serialize()),
        // The viewport is fixed (vault/CONTRACT.md §3.5), so `matches`
        // never changes and no `change` event fires.
        "get_matches" => IdlValue::Bool(m.list.matches(&Environment::default())),
        "addListener" | "removeListener" => {
            if inv.object(0).is_none() {
                return Ok(IdlValue::Undefined);
            }
            let op = if member == "addListener" {
                Op::EventTarget_addEventListener
            } else {
                Op::EventTarget_removeEventListener
            };
            let mut forwarded = Invocation {
                op,
                overload: 0,
                iface: inv.iface,
                this: inv.this.clone(),
                args: vec![string("change"), inv.arg(0).clone(), IdlValue::Missing],
                new_target: None,
            };
            return super::imp::call(agent, &mut forwarded);
        }
        _ => return not_implemented(agent, inv.op),
    })
}
