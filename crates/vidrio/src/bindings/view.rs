//! The CSSOM View members over [`crate::view`]: `getClientRects` and
//! `getBoundingClientRect` (with the `DOMRect`, `DOMRectReadOnly` and
//! `DOMRectList` objects they return), `offset*`, `client*`, `scroll*`,
//! `scroll`/`scrollTo`/`scrollBy`/`scrollIntoView` of elements and of the
//! window, `elementFromPoint`, `elementsFromPoint`,
//! `caretPositionFromPoint` (`CaretPosition`), `scrollingElement`,
//! `innerWidth`/`innerHeight`, `scrollX`/`scrollY` (and `pageXOffset`/
//! `pageYOffset`), `outerWidth`/`outerHeight` (the top-level viewport),
//! `screenX`/`screenY`/`screenLeft`/`screenTop` (always 0),
//! `devicePixelRatio` (always 1) and `screen`.
//!
//! Scrolling is instant whatever `behavior` says, so the promises the
//! scroll methods return are already resolved with
//! `{interrupted: false}`.

use std::cell::Cell;
use std::rc::Rc;

use vjs::{Agent, JsResult, Value};

use super::RealmState;
use super::platform::Native;
use super::value::{IdlValue, Invocation};
use super::wrappers::wrap_node;
use crate::dom::{DocRef, NodeId};
use crate::layout::fragment::Rect;
use crate::view::{self, Align};

type R = JsResult<IdlValue>;

/// A `DOMRect` or `DOMRectReadOnly`.
pub struct RectData {
    pub x: Cell<f64>,
    pub y: Cell<f64>,
    pub width: Cell<f64>,
    pub height: Cell<f64>,
}

/// A `DOMRectList`.
pub struct RectListData(pub Vec<Rect>);

/// A `CaretPosition`.
pub struct CaretData {
    pub doc: DocRef,
    pub node: NodeId,
    pub offset: usize,
    pub rect: Option<Rect>,
}

/// The color depth `screen` reports.
const COLOR_DEPTH: u64 = 24;

/// Runs the CSSOM View member `inv.op`, or returns `None` if it is not
/// one.
pub fn call(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> Option<R> {
    let name = inv.op.name();
    let (owner, member) = name.split_once('_')?;
    match owner {
        "Element" => element(agent, state, inv, member),
        "HTMLElement" => html_element(agent, inv, member),
        "Document" => document(agent, state, inv, member),
        "Window" => window(agent, state, inv, member),
        "Screen" => Some(screen(member)),
        "CaretPosition" => Some(caret(agent, state, inv, member)),
        "DOMRect" | "DOMRectReadOnly"
            if member != "constructor" && !member.starts_with("static_") =>
        {
            Some(rect(agent, inv, member))
        }
        "DOMRectList" => Some(rect_list(agent, state, inv, member)),
        _ => None,
    }
}

// ----- helpers ------------------------------------------------------------------

fn this_node(inv: &Invocation) -> Option<(DocRef, NodeId)> {
    let p = inv.this.as_ref()?;
    let native = p.get().native.borrow();
    match &*native {
        Native::Node(d, n) => Some((d.clone(), *n)),
        _ => None,
    }
}

fn wrap_opt(agent: &mut Agent, doc: &DocRef, node: Option<NodeId>) -> IdlValue {
    match node {
        Some(n) => IdlValue::Object(wrap_node(agent, doc, n)),
        None => IdlValue::Null,
    }
}

fn long(v: f64) -> IdlValue {
    IdlValue::Int(v.round() as i64)
}

fn new_rect(agent: &mut Agent, state: &RealmState, r: Rect) -> IdlValue {
    let data = Rc::new(RectData {
        x: Cell::new(r.x),
        y: Cell::new(r.y),
        width: Cell::new(r.width),
        height: Cell::new(r.height),
    });
    IdlValue::Object(crate::fetch::create_object(
        agent,
        state,
        "DOMRect",
        Native::Data(data),
        None,
    ))
}

/// A promise already resolved with `{interrupted: false}`.
fn scroll_result(agent: &mut Agent) -> R {
    let o = vjs::ops::object_create(agent);
    let key = agent.heap.key_from_str("interrupted");
    vjs::ops::create_data_property_or_throw(agent, o, key, Value::Boolean(false))?;
    let ctor = agent.intrinsic(vjs::realm::Intrinsic::Promise);
    let p = vjs::interp::promise::promise_resolve(agent, ctor, Value::Object(o))?;
    Ok(p.as_object().map_or(IdlValue::Undefined, IdlValue::Object))
}

/// The `left` and `top` of a scroll call: from a `ScrollToOptions`
/// dictionary, or the two numbers of the other overload.
fn scroll_args(inv: &Invocation) -> (Option<f64>, Option<f64>) {
    let member = |d: &super::value::Dict, n: &str| {
        d.get(n)
            .filter(|v| !matches!(v, IdlValue::Missing | IdlValue::Undefined))
            .map(IdlValue::as_f64)
    };
    match inv.arg(0) {
        IdlValue::Dictionary(d) => (member(d, "left"), member(d, "top")),
        IdlValue::Missing | IdlValue::Undefined | IdlValue::Null => (None, None),
        v => (Some(v.as_f64()), Some(inv.arg(1).as_f64())),
    }
}

fn finite(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

/// `scroll`/`scrollTo` (`by` false) or `scrollBy` (`by` true) of the
/// scroll container `node` ([`NodeId::DOCUMENT`]: the viewport).
fn scroll_call(agent: &mut Agent, doc: &DocRef, node: NodeId, inv: &Invocation, by: bool) -> R {
    let (left, top) = scroll_args(inv);
    let (cx, cy) = view::scroll_position(doc, node);
    let (x, y) = if by {
        (
            cx + finite(left.unwrap_or(0.0)),
            cy + finite(top.unwrap_or(0.0)),
        )
    } else {
        (left.map_or(cx, finite), top.map_or(cy, finite))
    };
    view::scroll_to(doc, node, x, y);
    scroll_result(agent)
}

fn align_of(s: Option<&str>, default: Align) -> Align {
    match s {
        Some("start") => Align::Start,
        Some("center") => Align::Center,
        Some("end") => Align::End,
        Some("nearest") => Align::Nearest,
        _ => default,
    }
}

// ----- members ------------------------------------------------------------------

fn element(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> Option<R> {
    let known = matches!(
        member,
        "getClientRects"
            | "getBoundingClientRect"
            | "scrollIntoView"
            | "scroll"
            | "scrollTo"
            | "scrollBy"
            | "get_scrollTop"
            | "set_scrollTop"
            | "get_scrollLeft"
            | "set_scrollLeft"
            | "get_scrollWidth"
            | "get_scrollHeight"
            | "get_clientTop"
            | "get_clientLeft"
            | "get_clientWidth"
            | "get_clientHeight"
    );
    if !known {
        return None;
    }
    let Some((doc, node)) = this_node(inv) else {
        return Some(agent.type_error("Illegal invocation"));
    };
    let target = view::scroll_target(&doc.borrow(), node);
    Some(Ok(match member {
        "getClientRects" => {
            let rects = view::client_rects(&doc, node);
            IdlValue::Object(crate::fetch::create_object(
                agent,
                state,
                "DOMRectList",
                Native::Data(Rc::new(RectListData(rects))),
                None,
            ))
        }
        "getBoundingClientRect" => {
            let r = view::bounding_client_rect(&doc, node);
            new_rect(agent, state, r)
        }
        "scrollIntoView" => {
            let (block, inline) = match inv.arg(0) {
                IdlValue::Bool(false) => (Align::End, Align::Nearest),
                IdlValue::Dictionary(d) => (
                    align_of(d.str("block"), Align::Start),
                    align_of(d.str("inline"), Align::Nearest),
                ),
                _ => (Align::Start, Align::Nearest),
            };
            view::scroll_into_view(&doc, node, block, inline);
            return Some(scroll_result(agent));
        }
        "scroll" | "scrollTo" => return Some(scroll_call(agent, &doc, target, inv, false)),
        "scrollBy" => return Some(scroll_call(agent, &doc, target, inv, true)),
        "get_scrollTop" => IdlValue::Double(view::scroll_position(&doc, target).1),
        "get_scrollLeft" => IdlValue::Double(view::scroll_position(&doc, target).0),
        "set_scrollTop" | "set_scrollLeft" => {
            let v = finite(inv.arg(0).as_f64());
            let (x, y) = view::scroll_position(&doc, target);
            if member == "set_scrollTop" {
                view::scroll_to(&doc, target, x, v);
            } else {
                view::scroll_to(&doc, target, v, y);
            }
            IdlValue::Undefined
        }
        "get_scrollWidth" => long(view::scroll_size(&doc, node).0),
        "get_scrollHeight" => long(view::scroll_size(&doc, node).1),
        m => {
            let (top, left, width, height) = view::client_metrics(&doc, node);
            long(match m {
                "get_clientTop" => top,
                "get_clientLeft" => left,
                "get_clientWidth" => width,
                _ => height,
            })
        }
    }))
}

fn html_element(agent: &mut Agent, inv: &Invocation, member: &str) -> Option<R> {
    if !member.starts_with("get_offset") {
        return None;
    }
    let Some((doc, node)) = this_node(inv) else {
        return Some(agent.type_error("Illegal invocation"));
    };
    if member == "get_offsetParent" {
        let p = view::offset_parent(&doc, node);
        return Some(Ok(wrap_opt(agent, &doc, p)));
    }
    let (top, left, width, height) = view::offset_metrics(&doc, node);
    Some(Ok(long(match member {
        "get_offsetTop" => top,
        "get_offsetLeft" => left,
        "get_offsetWidth" => width,
        _ => height,
    })))
}

fn document(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    if !matches!(
        member,
        "elementFromPoint"
            | "elementsFromPoint"
            | "caretPositionFromPoint"
            | "get_scrollingElement"
    ) {
        return None;
    }
    let Some((doc, _)) = this_node(inv) else {
        return Some(agent.type_error("Illegal invocation"));
    };
    let (x, y) = (inv.arg(0).as_f64(), inv.arg(1).as_f64());
    Some(Ok(match member {
        "elementFromPoint" => {
            let e = view::element_from_point(&doc, x, y);
            wrap_opt(agent, &doc, e)
        }
        "elementsFromPoint" => {
            let list = view::elements_from_point(&doc, x, y);
            IdlValue::Sequence(
                list.into_iter()
                    .map(|n| IdlValue::Object(wrap_node(agent, &doc, n)))
                    .collect(),
            )
        }
        "caretPositionFromPoint" => match view::caret_position_from_point(&doc, x, y) {
            Some((node, offset, rect)) => IdlValue::Object(crate::fetch::create_object(
                agent,
                state,
                "CaretPosition",
                Native::Data(Rc::new(CaretData {
                    doc: doc.clone(),
                    node,
                    offset,
                    rect,
                })),
                None,
            )),
            None => IdlValue::Null,
        },
        _ => {
            let e = view::scrolling_element(&doc.borrow());
            wrap_opt(agent, &doc, e)
        }
    }))
}

fn window(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> Option<R> {
    if !matches!(
        member,
        "get_innerWidth"
            | "get_innerHeight"
            | "get_scrollX"
            | "get_pageXOffset"
            | "get_scrollY"
            | "get_pageYOffset"
            | "scroll"
            | "scrollTo"
            | "scrollBy"
            | "get_devicePixelRatio"
            | "get_screen"
            | "get_outerWidth"
            | "get_outerHeight"
            | "get_screenX"
            | "get_screenLeft"
            | "get_screenY"
            | "get_screenTop"
    ) {
        return None;
    }
    if member == "get_devicePixelRatio" {
        return Some(Ok(IdlValue::Double(1.0)));
    }
    // The client window has no browser UI around the top-level viewport
    // and sits at the origin of the screen (vault/CONTRACT.md §3.5).
    match member {
        "get_outerWidth" => return Some(Ok(IdlValue::Int(i64::from(view::viewport().0)))),
        "get_outerHeight" => return Some(Ok(IdlValue::Int(i64::from(view::viewport().1)))),
        "get_screenX" | "get_screenLeft" | "get_screenY" | "get_screenTop" => {
            return Some(Ok(IdlValue::Int(0)));
        }
        _ => {}
    }
    if member == "get_screen" {
        // [SameObject]: the window keeps its `Screen`.
        let cached = inv.this.as_ref().and_then(|p| p.get().slot("screen"));
        if let Some(Value::Object(o)) = cached {
            return Some(Ok(IdlValue::Object(o)));
        }
        let o = crate::fetch::create_object(agent, state, "Screen", Native::None, None);
        if let Some(p) = &inv.this {
            p.get().set_slot("screen", Value::Object(o));
        }
        return Some(Ok(IdlValue::Object(o)));
    }
    let Some(doc) = state.document() else {
        return Some(agent.type_error("The window has no document."));
    };
    Some(match member {
        "get_innerWidth" => Ok(IdlValue::Int(i64::from(view::viewport_of(&doc).0))),
        "get_innerHeight" => Ok(IdlValue::Int(i64::from(view::viewport_of(&doc).1))),
        "get_scrollX" | "get_pageXOffset" => Ok(IdlValue::Double(
            view::scroll_position(&doc, NodeId::DOCUMENT).0,
        )),
        "get_scrollY" | "get_pageYOffset" => Ok(IdlValue::Double(
            view::scroll_position(&doc, NodeId::DOCUMENT).1,
        )),
        "scrollBy" => scroll_call(agent, &doc, NodeId::DOCUMENT, inv, true),
        _ => scroll_call(agent, &doc, NodeId::DOCUMENT, inv, false),
    })
}

fn screen(member: &str) -> R {
    let (w, h) = view::viewport();
    Ok(match member {
        "get_width" | "get_availWidth" => IdlValue::Int(i64::from(w)),
        "get_height" | "get_availHeight" => IdlValue::Int(i64::from(h)),
        _ => IdlValue::UInt(COLOR_DEPTH),
    })
}

fn caret(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    let c = crate::fetch::this_data::<CaretData>(agent, inv)?;
    Ok(match member {
        "get_offsetNode" => IdlValue::Object(wrap_node(agent, &c.doc, c.node)),
        "get_offset" => IdlValue::UInt(c.offset as u64),
        _ => match c.rect {
            Some(r) => new_rect(agent, state, r),
            None => IdlValue::Null,
        },
    })
}

fn rect(agent: &mut Agent, inv: &Invocation, member: &str) -> R {
    let r = crate::fetch::this_data::<RectData>(agent, inv)?;
    let (x, y, w, h) = (r.x.get(), r.y.get(), r.width.get(), r.height.get());
    // `min`/`max` that keep NaN (Geometry §4: "NaN-safe").
    let nan_min = |a: f64, b: f64| {
        if a.is_nan() || b.is_nan() {
            f64::NAN
        } else {
            a.min(b)
        }
    };
    let nan_max = |a: f64, b: f64| {
        if a.is_nan() || b.is_nan() {
            f64::NAN
        } else {
            a.max(b)
        }
    };
    let value = |m: &str| match m {
        "x" => x,
        "y" => y,
        "width" => w,
        "height" => h,
        "top" => nan_min(y, y + h),
        "right" => nan_max(x, x + w),
        "bottom" => nan_max(y, y + h),
        _ => nan_min(x, x + w),
    };
    if let Some(m) = member.strip_prefix("get_") {
        return Ok(IdlValue::Double(value(m)));
    }
    if let Some(m) = member.strip_prefix("set_") {
        let v = inv.arg(0).as_f64();
        match m {
            "x" => r.x.set(v),
            "y" => r.y.set(v),
            "width" => r.width.set(v),
            _ => r.height.set(v),
        }
        return Ok(IdlValue::Undefined);
    }
    // toJSON
    let o = vjs::ops::object_create(agent);
    for k in [
        "x", "y", "width", "height", "top", "right", "bottom", "left",
    ] {
        let key = agent.heap.key_from_str(k);
        vjs::ops::create_data_property_or_throw(agent, o, key, Value::Number(value(k)))?;
    }
    Ok(IdlValue::Object(o))
}

fn rect_list(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    let l = crate::fetch::this_data::<RectListData>(agent, inv)?;
    Ok(match member {
        "get_length" | "index_count" => IdlValue::UInt(l.0.len() as u64),
        _ => match l.0.get(inv.usize(0)) {
            Some(r) => new_rect(agent, state, *r),
            None => IdlValue::Null,
        },
    })
}
