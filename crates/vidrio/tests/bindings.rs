//! Web IDL bindings between the DOM and vjs: the property layout of
//! generated interface objects and prototypes, integer conversions with
//! and without `[EnforceRange]`/`[Clamp]`, overload resolution, and one
//! wrapper per node across a garbage collection with its expandos.

use std::cell::RefCell;
use std::rc::Rc;

use vidrio::bindings::convert::to_idl;
use vidrio::bindings::generated::{Op, ids};
use vidrio::bindings::meta::{Argument, DefaultValue, Operation, Overload, Ty};
use vidrio::bindings::overload::resolve;
use vidrio::bindings::{IdlValue, WindowOptions, create_window_realm};
use vidrio::dom::DocRef;
use vidrio::html::parse_document;
use vjs::gc::Realm;
use vjs::{Agent, Value};

/// An agent with a window realm over `html`.
fn window(html: &str) -> (Agent, Realm) {
    let mut agent = Agent::new();
    let doc: DocRef = Rc::new(RefCell::new(parse_document(html, true)));
    let realm = create_window_realm(&mut agent, doc, WindowOptions::default());
    (agent, realm)
}

/// Runs `source` and returns its completion value as a string (or the
/// thrown value, prefixed with `threw: `).
fn run(agent: &mut Agent, realm: Realm, source: &str) -> String {
    match vjs::evaluate_script_in(agent, realm, source) {
        Ok(v) => to_string(agent, v),
        Err(e) => format!("threw: {}", to_string(agent, e.0)),
    }
}

fn to_string(agent: &mut Agent, v: Value) -> String {
    match vjs::ops::to_string(agent, v) {
        Ok(s) => agent.heap.string_to_rust(s),
        Err(_) => "<unprintable>".to_string(),
    }
}

const PAGE: &str = "<!doctype html><html><head><title> a \n title </title></head><body><p id=x>text</p></body></html>";

#[test]
fn generated_property_descriptors() {
    let (mut agent, realm) = window(PAGE);
    let r = run(
        &mut agent,
        realm,
        r#"(function () {
  var d = Object.getOwnPropertyDescriptor(Node.prototype, "firstChild");
  if (!d || typeof d.get !== "function" || d.set !== undefined || !d.enumerable || !d.configurable) return "firstChild accessor";
  if (d.get.name !== "get firstChild" || d.get.length !== 0) return "getter name";
  if (Object.getOwnPropertyDescriptor(document.body, "firstChild") !== undefined) return "attribute on the instance";
  var v = Object.getOwnPropertyDescriptor(Node.prototype, "nodeValue");
  if (typeof v.set !== "function" || v.set.name !== "set nodeValue" || v.set.length !== 1) return "nodeValue setter";
  var m = Object.getOwnPropertyDescriptor(Node.prototype, "appendChild");
  if (!m.writable || !m.enumerable || !m.configurable || m.value.length !== 1 || m.value.name !== "appendChild") return "appendChild";
  var o = Object.getOwnPropertyDescriptor(Document.prototype, "createElement");
  if (o.value.length !== 1) return "createElement length";
  var c = Object.getOwnPropertyDescriptor(Node, "ELEMENT_NODE");
  if (c.value !== 1 || c.writable || !c.enumerable || c.configurable) return "constant on the interface object";
  if (Node.prototype.DOCUMENT_NODE !== 9) return "constant on the prototype";
  if (Event.length !== 1 || EventTarget.length !== 0 || MutationObserver.length !== 1) return "constructor lengths";
  if (Node.length !== 0 || Text.length !== 0 || DOMException.length !== 0) return "lengths without required arguments";
  var l = Object.getOwnPropertyDescriptor(Event, "length");
  if (l.writable || l.enumerable || !l.configurable) return "length descriptor";
  var n = Object.getOwnPropertyDescriptor(Event, "name");
  if (n.value !== "Event" || n.writable || n.enumerable || !n.configurable) return "name descriptor";
  var p = Object.getOwnPropertyDescriptor(Node, "prototype");
  if (p.writable || p.enumerable || p.configurable) return "prototype descriptor";
  var k = Object.getOwnPropertyDescriptor(Node.prototype, "constructor");
  if (k.value !== Node || !k.writable || k.enumerable || !k.configurable) return "constructor descriptor";
  if (Object.getPrototypeOf(Element) !== Node || Object.getPrototypeOf(Element.prototype) !== Node.prototype) return "inheritance";
  if (Object.getPrototypeOf(EventTarget) !== Function.prototype) return "root interface object";
  if (Object.getPrototypeOf(DOMException.prototype) !== Error.prototype) return "DOMException prototype";
  if (Object.prototype.toString.call(Node.prototype) !== "[object Node]") return "toStringTag";
  if (Element.prototype[Symbol.unscopables].append !== true) return "unscopables";
  var w = Object.getOwnPropertyDescriptor(window, "Node");
  if (!w.writable || w.enumerable || !w.configurable) return "global binding";
  try { Node(); return "call without new"; } catch (e) { if (!(e instanceof TypeError)) return "call error"; }
  try { new Node(); return "illegal constructor"; } catch (e) { if (!(e instanceof TypeError)) return "illegal constructor error"; }
  try { d.get.call({}); return "brand check"; } catch (e) { if (!(e instanceof TypeError)) return "brand check error"; }
  var e = new Event("x");
  var t = Object.getOwnPropertyDescriptor(e, "isTrusted");
  if (!t || t.configurable || typeof t.get !== "function") return "LegacyUnforgeable isTrusted";
  if (Object.getOwnPropertyDescriptor(Event.prototype, "isTrusted") !== undefined) return "isTrusted on the prototype";
  var x = new DOMException("m", "NotFoundError");
  if (x.code !== 8 || x.name !== "NotFoundError" || x.message !== "m" || DOMException.NOT_FOUND_ERR !== 8) return "DOMException";
  if (new DOMException("m", "EncodingError").code !== 0) return "DOMException without legacy code";
  return "ok";
})()"#,
    );
    assert_eq!(r, "ok");
}

#[test]
fn integer_conversions() {
    let (mut agent, _realm) = window(PAGE);
    let mut conv = |v: f64, ty: Ty| to_idl(&mut agent, Value::Number(v), ty).ok();
    // Without annotations: modulo 2^bits.
    assert_eq!(conv(300.0, Ty::Octet), Some(IdlValue::Int(44)));
    assert_eq!(conv(-1.0, Ty::Octet), Some(IdlValue::Int(255)));
    assert_eq!(conv(128.0, Ty::Byte), Some(IdlValue::Int(-128)));
    assert_eq!(
        conv(2147483648.0, Ty::Long),
        Some(IdlValue::Int(-2147483648))
    );
    assert_eq!(
        conv(-1.0, Ty::UnsignedLong),
        Some(IdlValue::Int(4294967295))
    );
    assert_eq!(conv(4294967298.5, Ty::UnsignedLong), Some(IdlValue::Int(2)));
    assert_eq!(conv(-3.7, Ty::Short), Some(IdlValue::Int(-3)));
    assert_eq!(conv(f64::NAN, Ty::Long), Some(IdlValue::Int(0)));
    assert_eq!(conv(f64::INFINITY, Ty::Long), Some(IdlValue::Int(0)));
    assert_eq!(
        conv(-1.0, Ty::UnsignedLongLong),
        Some(IdlValue::UInt(u64::MAX))
    );
    // [Clamp]: to the range, ties to even.
    assert_eq!(conv(300.0, Ty::Clamp(&Ty::Octet)), Some(IdlValue::Int(255)));
    assert_eq!(conv(-5.0, Ty::Clamp(&Ty::Octet)), Some(IdlValue::Int(0)));
    assert_eq!(conv(2.5, Ty::Clamp(&Ty::Octet)), Some(IdlValue::Int(2)));
    assert_eq!(conv(3.5, Ty::Clamp(&Ty::Octet)), Some(IdlValue::Int(4)));
    assert_eq!(conv(f64::NAN, Ty::Clamp(&Ty::Long)), Some(IdlValue::Int(0)));
    // [EnforceRange]: truncate, then throw outside the range or for
    // non-finite values.
    assert_eq!(
        conv(254.9, Ty::EnforceRange(&Ty::Octet)),
        Some(IdlValue::Int(254))
    );
    assert_eq!(
        conv(-0.9, Ty::EnforceRange(&Ty::Octet)),
        Some(IdlValue::Int(0))
    );
    assert_eq!(conv(256.0, Ty::EnforceRange(&Ty::Octet)), None);
    assert_eq!(conv(-1.0, Ty::EnforceRange(&Ty::UnsignedLong)), None);
    assert_eq!(conv(f64::NAN, Ty::EnforceRange(&Ty::Long)), None);
    assert_eq!(conv(f64::INFINITY, Ty::EnforceRange(&Ty::Long)), None);
    assert_eq!(
        conv(9007199254740992.0, Ty::EnforceRange(&Ty::LongLong)),
        None
    );
    assert_eq!(
        conv(9007199254740991.0, Ty::EnforceRange(&Ty::UnsignedLongLong)),
        Some(IdlValue::UInt(9007199254740991))
    );
}

#[test]
fn integer_conversions_through_members() {
    let (mut agent, realm) = window(PAGE);
    // `substringData(unsigned long offset, unsigned long count)`: 2^32 + 2
    // wraps to 2.
    let r = run(
        &mut agent,
        realm,
        r#"new Text("abcdef").substringData(4294967298, 2)"#,
    );
    assert_eq!(r, "cd");
    // `AbortSignal.timeout([EnforceRange] unsigned long long)`.
    let r = run(
        &mut agent,
        realm,
        r#"(function () {
  var errors = 0;
  for (var v of [-1, NaN, Infinity, 2 ** 53]) {
    try { AbortSignal.timeout(v); } catch (e) { if (e instanceof TypeError) errors++; }
  }
  return errors + " " + (AbortSignal.timeout(5.9) instanceof AbortSignal);
})()"#,
    );
    assert_eq!(r, "4 true");
}

/// `f(long a)`, `f(DOMString a, optional long b = 7)` and
/// `f(Node n, long b)`.
static OVERLOADED: Operation = Operation {
    name: "f",
    overloads: &[
        Overload {
            op: Op::Node_appendChild,
            index: 0,
            args: &[Argument {
                name: "a",
                ty: Ty::Long,
                optional: false,
                variadic: false,
                default: None,
            }],
            ret: Ty::Undefined,
        },
        Overload {
            op: Op::Node_insertBefore,
            index: 1,
            args: &[
                Argument {
                    name: "a",
                    ty: Ty::DomString,
                    optional: false,
                    variadic: false,
                    default: None,
                },
                Argument {
                    name: "b",
                    ty: Ty::Long,
                    optional: true,
                    variadic: false,
                    default: Some(DefaultValue::Number(7.0)),
                },
            ],
            ret: Ty::Undefined,
        },
        Overload {
            op: Op::Node_replaceChild,
            index: 2,
            args: &[
                Argument {
                    name: "n",
                    ty: Ty::Interface(ids::Node),
                    optional: false,
                    variadic: false,
                    default: None,
                },
                Argument {
                    name: "b",
                    ty: Ty::Long,
                    optional: false,
                    variadic: false,
                    default: None,
                },
            ],
            ret: Ty::Undefined,
        },
    ],
    length: 1,
    is_static: false,
    unforgeable: false,
    unscopable: false,
    secure_context: false,
    new_object: false,
};

#[test]
fn overload_resolution() {
    let (mut agent, realm) = window(PAGE);
    let s = Value::String(agent.heap.alloc_str("x"));
    let node = vjs::evaluate_script_in(&mut agent, realm, "document.body").expect("body");

    let r = resolve(&mut agent, &OVERLOADED, &[Value::Number(3.0)]).expect("long");
    assert_eq!(r.overload.index, 0);
    assert_eq!(r.args, vec![IdlValue::Int(3)]);

    let r = resolve(&mut agent, &OVERLOADED, &[s]).expect("string");
    assert_eq!(r.overload.index, 1);
    assert_eq!(
        r.args,
        vec![IdlValue::String("x".to_string()), IdlValue::Int(7)]
    );

    // A boolean is neither a number nor a string: the string fallback
    // comes first.
    let r = resolve(&mut agent, &OVERLOADED, &[Value::Boolean(true)]).expect("boolean");
    assert_eq!(r.overload.index, 1);

    let r = resolve(&mut agent, &OVERLOADED, &[s, Value::Number(1.0)]).expect("two");
    assert_eq!(r.overload.index, 1);
    assert_eq!(r.args[1], IdlValue::Int(1));

    let r = resolve(&mut agent, &OVERLOADED, &[node, Value::Number(2.0)]).expect("node");
    assert_eq!(r.overload.index, 2);
    assert_eq!(r.overload.op, Op::Node_replaceChild);

    // Extra arguments are ignored; too few is a TypeError.
    let r = resolve(
        &mut agent,
        &OVERLOADED,
        &[s, Value::Number(1.0), Value::Null],
    )
    .expect("extra");
    assert_eq!(r.args.len(), 2);
    assert!(resolve(&mut agent, &OVERLOADED, &[]).is_err());
}

#[test]
fn wrappers_survive_collection_with_expandos() {
    let (mut agent, realm) = window(PAGE);
    let r = run(
        &mut agent,
        realm,
        r#"(function () {
  var p = document.getElementById("x");
  p.expando = 42;
  var s = document.createElement("span");
  s.marker = "m";
  document.body.appendChild(s);
  for (var i = 0; i < 1000; i++) ({ garbage: [i] });
  return (p === document.body.firstChild) + " " + (document.body === document.body);
})()"#,
    );
    assert_eq!(r, "true true");
    // No script holds the wrappers now: only the document's node→wrapper
    // table (a root while the document is active) keeps them.
    let report = agent.collect_garbage(&[]);
    assert!(report.is_some(), "the collector ran");
    let r = run(
        &mut agent,
        realm,
        r#"(function () {
  var p = document.getElementById("x");
  var s = document.body.lastChild;
  return [p.expando, s.marker, p === document.body.firstChild, s.localName,
          document.querySelector("span") === s].join(" ");
})()"#,
    );
    assert_eq!(r, "42 m true span true");
}

#[test]
fn dom_members_reach_the_native_implementation() {
    let (mut agent, realm) = window(PAGE);
    let r = run(
        &mut agent,
        realm,
        r#"(function () {
  var out = [];
  out.push(document.title);
  out.push(document.body.childNodes.length, document.body.childNodes[0].id);
  var d = document.createElement("div");
  d.className = "a b";
  out.push(d.classList.length, d.classList.contains("b"), String(d.classList));
  d.classList = "c";
  out.push(d.getAttribute("class"));
  var calls = 0;
  d.addEventListener("ping", function (e) { calls++; out.push(e.target === d, this === d); });
  d.dispatchEvent(new Event("ping"));
  out.push(calls);
  try { document.createElement("1"); } catch (e) { out.push(e.name, e.code, e instanceof DOMException); }
  var other = document.implementation.createHTMLDocument("t");
  var x = document.getElementById("x");
  x.expando = 1;
  var moved = other.body.appendChild(x);
  out.push(moved === x, x.expando, x.ownerDocument === other, document.getElementById("x"));
  var keys = [];
  for (var n of document.body.childNodes) keys.push(n.nodeName);
  out.push(keys.length);
  return out.join(",");
})()"#,
    );
    assert_eq!(
        r,
        "a title,1,x,2,true,a b,c,true,true,1,InvalidCharacterError,5,true,true,1,true,,0"
    );
}

#[test]
fn inline_style_reflects_the_style_attribute() {
    let (mut agent, realm) = window(PAGE);
    let r = run(
        &mut agent,
        realm,
        r#"(function () {
  var out = [];
  var d = document.createElement("div");
  var s = d.style;
  out.push(s === d.style, s instanceof CSSStyleProperties, s.length, s.cssText === "");
  s.setProperty("display", "none");
  out.push(d.getAttribute("style"), s.getPropertyValue("DISPLAY"));
  d.setAttribute("style", "color: red !important; bogus");
  out.push(s.length, s.item(0), s[0], s.getPropertyPriority("color"));
  d.style = "float: left";
  out.push(s.cssFloat, s.cssText);
  out.push(s.removeProperty("float"), d.getAttribute("style") === "", s.parentRule);
  return out.join(",");
})()"#,
    );
    assert_eq!(
        r,
        "true,true,0,true,display: none;,none,1,color,color,important,left,float: left;,left,true,"
    );
}

#[test]
fn dataset_maps_data_attributes() {
    let (mut agent, realm) = window(PAGE);
    let r = run(
        &mut agent,
        realm,
        r#"(function () {
  var out = [];
  var d = document.createElement("div");
  d.setAttribute("data-run-type", "url");
  out.push(d.dataset === d.dataset, d.dataset.runType, Object.keys(d.dataset).join("|"));
  d.dataset.fooBar = "x";
  out.push(d.getAttribute("data-foo-bar"));
  try { d.dataset["a-b"] = "y"; } catch (e) { out.push(e.name); }
  delete d.dataset.runType;
  out.push(d.hasAttribute("data-run-type"), "runType" in d.dataset);
  return out.join(",");
})()"#,
    );
    assert_eq!(r, "true,url,runType,x,SyntaxError,false,false");
}
