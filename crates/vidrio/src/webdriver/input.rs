//! User input of WebDriver §15 ("Actions") and §12.4 ("Element
//! interaction"): action sequences of pause, pointer, wheel and key
//! sources processed tick by tick, Element Click and Element Send Keys.
//! Every action becomes trusted DOM events (`pointer*`, `mouse*`,
//! `click`, `wheel`, `key*`, `focus`/`blur`, `input`) dispatched at the
//! target.
//!
//! WebDriver actions do not hit test: a pointer targets the element of
//! its last `pointerMove` with an element origin (or the element last
//! clicked), and otherwise the `body` (or the root element) of the
//! current browsing context's document. Coordinates are the ones the
//! actions give, relative to the viewport; an element origin counts as
//! the viewport's origin (LIMITATIONS.md). The X window's input
//! ([`window_pointer_move`] and the others) goes through the same event
//! code with hit testing over the layout of `crate::view`.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use vjs::Value;

use super::Session;
use super::error::{ErrorCode, WdError, WdResult, invalid_argument};
use super::json::Json;
use super::script;
use crate::bindings::platform::platform_ref;
use crate::browser::{Browser, ContextId, ua};
use crate::dom::events::{Event, EventInit, EventInterface, Target, dispatch};
use crate::dom::{DocRef, Document, NodeId};
use crate::paint::{VIEWPORT_HEIGHT, VIEWPORT_WIDTH};

/// The modifier keys that are down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

/// The input state of a session (WebDriver §15.3 "input state"): one
/// mouse pointer and one keyboard.
#[derive(Default)]
pub struct InputState {
    /// The pointer position in viewport coordinates.
    pub x: f64,
    pub y: f64,
    /// The pressed mouse buttons (bit 0 = button 0, …).
    pub buttons: u16,
    /// The pressed keys, as their raw values.
    pub pressed: Vec<char>,
    pub modifiers: Modifiers,
    /// The element under the pointer.
    pub pointer_target: Option<(Weak<RefCell<Document>>, NodeId)>,
    /// The element that received the last `pointerdown`.
    pub down_target: Option<(Weak<RefCell<Document>>, NodeId)>,
    /// The focused element (the target of key events).
    pub focus: Option<(Weak<RefCell<Document>>, NodeId)>,
}

/// A value stored on an event's wrapper for its attributes.
enum Slot {
    Num(f64),
    Bool(bool),
    Str(String),
}

/// Fires a trusted, composed event `type_` of `interface` at `node`;
/// `slots` become attributes of its wrapper. Returns false if it was
/// canceled.
#[allow(clippy::too_many_arguments)]
fn fire(
    browser: &mut Browser,
    doc: &DocRef,
    node: NodeId,
    type_: &str,
    interface: EventInterface,
    bubbles: bool,
    cancelable: bool,
    slots: Vec<(&'static str, Slot)>,
) -> bool {
    let Some(ua) = ua() else {
        return true;
    };
    let agent = &mut browser.agent;
    let Some(state) = crate::browser::window::state_of_document(agent, &ua, doc) else {
        return true;
    };
    let now = crate::browser::window::window_now(&state);
    let init = EventInit {
        bubbles,
        cancelable,
        composed: true,
    };
    let mut e = Event::new_trusted(type_, init, now);
    e.interface = interface;
    let ev = e.into_ref();
    let o = crate::bindings::host::event_wrapper(agent, &state, &ev);
    let values: Vec<(&'static str, Value)> = slots
        .into_iter()
        .map(|(k, v)| {
            let v = match v {
                Slot::Num(n) => Value::Number(n),
                Slot::Bool(b) => Value::Boolean(b),
                Slot::Str(s) => Value::String(agent.heap.alloc_str(&s)),
            };
            (k, v)
        })
        .collect();
    if let Some(p) = platform_ref(agent, Value::Object(o)) {
        for (k, v) in values {
            p.get().set_slot(k, v);
        }
    }
    let r = {
        let mut host = crate::bindings::host::JsHost::with_state(agent, doc.clone(), state.clone());
        dispatch(doc, &mut host, &ev, Target::Node(node), false)
    };
    crate::browser::event_loop::clean_up_after_running_script(agent);
    r
}

fn alive(slot: &Option<(Weak<RefCell<Document>>, NodeId)>, doc: &DocRef) -> Option<NodeId> {
    let (d, n) = slot.as_ref()?;
    let d = d.upgrade()?;
    (Rc::ptr_eq(&d, doc) && d.borrow().is_connected(*n)).then_some(*n)
}

/// The `body`, or else the root element, of `doc`.
fn default_target(doc: &DocRef) -> Option<NodeId> {
    let d = doc.borrow();
    d.body().or_else(|| d.document_element())
}

fn active_document(ctx: ContextId) -> WdResult<DocRef> {
    ua().and_then(|u| u.context(ctx))
        .and_then(|c| c.active_document())
        .ok_or_else(|| WdError::new(ErrorCode::NoSuchWindow, "no such window"))
}

fn pointer_target(input: &InputState, doc: &DocRef) -> Option<NodeId> {
    alive(&input.pointer_target, doc).or_else(|| default_target(doc))
}

fn key_target(input: &InputState, doc: &DocRef) -> Option<NodeId> {
    alive(&input.focus, doc).or_else(|| default_target(doc))
}

fn modifier_slots(m: Modifiers) -> Vec<(&'static str, Slot)> {
    vec![
        ("shiftKey", Slot::Bool(m.shift)),
        ("ctrlKey", Slot::Bool(m.ctrl)),
        ("altKey", Slot::Bool(m.alt)),
        ("metaKey", Slot::Bool(m.meta)),
    ]
}

/// The attribute values of a mouse event at the pointer.
fn mouse_slots(input: &InputState, button: i16, detail: f64) -> Vec<(&'static str, Slot)> {
    let i = &*input;
    let mut s = vec![
        ("clientX", Slot::Num(i.x)),
        ("clientY", Slot::Num(i.y)),
        ("screenX", Slot::Num(i.x)),
        ("screenY", Slot::Num(i.y)),
        ("pageX", Slot::Num(i.x)),
        ("pageY", Slot::Num(i.y)),
        ("x", Slot::Num(i.x)),
        ("y", Slot::Num(i.y)),
        ("offsetX", Slot::Num(i.x)),
        ("offsetY", Slot::Num(i.y)),
        ("button", Slot::Num(f64::from(button))),
        ("buttons", Slot::Num(f64::from(i.buttons))),
        ("detail", Slot::Num(detail)),
    ];
    s.extend(modifier_slots(i.modifiers));
    s
}

fn pointer_slots(input: &InputState, button: i16) -> Vec<(&'static str, Slot)> {
    let mut s = mouse_slots(input, button, 0.0);
    s.push(("pointerId", Slot::Num(1.0)));
    s.push(("pointerType", Slot::Str("mouse".to_string())));
    s.push(("isPrimary", Slot::Bool(true)));
    s.push(("width", Slot::Num(1.0)));
    s.push(("height", Slot::Num(1.0)));
    s.push((
        "pressure",
        Slot::Num(if input.buttons != 0 { 0.5 } else { 0.0 }),
    ));
    s
}

const POINTER: EventInterface = EventInterface::Other("PointerEvent");

/// Moves the pointer to `(x, y)` over `target`: `pointerout`/`mouseout`
/// and `pointerleave`/`mouseleave` at the old target if it changes,
/// `pointerover`/`mouseover` and `pointerenter`/`mouseenter` at the new
/// one, then `pointermove` and `mousemove`.
fn move_pointer(
    browser: &mut Browser,
    input: &mut InputState,
    doc: &DocRef,
    target: NodeId,
    x: f64,
    y: f64,
) {
    let old = pointer_target(input, doc);
    input.x = x;
    input.y = y;
    input.pointer_target = Some((Rc::downgrade(doc), target));
    if old != Some(target) {
        if let Some(old) = old {
            fire(
                browser,
                doc,
                old,
                "pointerout",
                POINTER,
                true,
                true,
                pointer_slots(input, -1),
            );
            fire(
                browser,
                doc,
                old,
                "pointerleave",
                POINTER,
                false,
                false,
                pointer_slots(input, -1),
            );
            fire(
                browser,
                doc,
                old,
                "mouseout",
                EventInterface::MouseEvent,
                true,
                true,
                mouse_slots(input, 0, 0.0),
            );
            fire(
                browser,
                doc,
                old,
                "mouseleave",
                EventInterface::MouseEvent,
                false,
                false,
                mouse_slots(input, 0, 0.0),
            );
        }
        fire(
            browser,
            doc,
            target,
            "pointerover",
            POINTER,
            true,
            true,
            pointer_slots(input, -1),
        );
        fire(
            browser,
            doc,
            target,
            "pointerenter",
            POINTER,
            false,
            false,
            pointer_slots(input, -1),
        );
        fire(
            browser,
            doc,
            target,
            "mouseover",
            EventInterface::MouseEvent,
            true,
            true,
            mouse_slots(input, 0, 0.0),
        );
        fire(
            browser,
            doc,
            target,
            "mouseenter",
            EventInterface::MouseEvent,
            false,
            false,
            mouse_slots(input, 0, 0.0),
        );
    }
    let prevented = !fire(
        browser,
        doc,
        target,
        "pointermove",
        POINTER,
        true,
        true,
        pointer_slots(input, -1),
    );
    if !prevented {
        fire(
            browser,
            doc,
            target,
            "mousemove",
            EventInterface::MouseEvent,
            true,
            true,
            mouse_slots(input, 0, 0.0),
        );
    }
}

/// Presses mouse button `button` over the pointer target.
fn pointer_down(browser: &mut Browser, input: &mut InputState, doc: &DocRef, button: u16) {
    let bit = 1u16.checked_shl(u32::from(button)).unwrap_or(0);
    if input.buttons & bit != 0 {
        return;
    }
    let Some(target) = pointer_target(input, doc) else {
        return;
    };
    input.buttons |= bit;
    input.down_target = Some((Rc::downgrade(doc), target));
    let b = button as i16;
    let prevented = !fire(
        browser,
        doc,
        target,
        "pointerdown",
        POINTER,
        true,
        true,
        pointer_slots(input, b),
    );
    if !prevented {
        fire(
            browser,
            doc,
            target,
            "mousedown",
            EventInterface::MouseEvent,
            true,
            true,
            mouse_slots(input, b, 1.0),
        );
    }
    if button == 0 {
        focus(browser, input, doc, target);
    }
}

/// Releases mouse button `button`: `pointerup`, `mouseup`, and a
/// `click` (button 0) or `auxclick` when the press was on the same
/// element.
fn pointer_up(browser: &mut Browser, input: &mut InputState, doc: &DocRef, button: u16) {
    let bit = 1u16.checked_shl(u32::from(button)).unwrap_or(0);
    if input.buttons & bit == 0 {
        return;
    }
    input.buttons &= !bit;
    let Some(target) = pointer_target(input, doc) else {
        return;
    };
    let b = button as i16;
    let prevented = !fire(
        browser,
        doc,
        target,
        "pointerup",
        POINTER,
        true,
        true,
        pointer_slots(input, b),
    );
    if !prevented {
        fire(
            browser,
            doc,
            target,
            "mouseup",
            EventInterface::MouseEvent,
            true,
            true,
            mouse_slots(input, b, 1.0),
        );
    }
    let same = alive(&input.down_target, doc) == Some(target);
    input.down_target = None;
    if same {
        let name = if button == 0 { "click" } else { "auxclick" };
        fire(
            browser,
            doc,
            target,
            name,
            POINTER,
            true,
            true,
            mouse_slots(input, b, 1.0),
        );
    }
}

/// Moves the focus to `target`: `blur`/`focusout` at the old element,
/// `focus`/`focusin` at the new one.
fn focus(browser: &mut Browser, input: &mut InputState, doc: &DocRef, target: NodeId) {
    let old = alive(&input.focus, doc);
    if old == Some(target) {
        return;
    }
    input.focus = Some((Rc::downgrade(doc), target));
    let focus_iface = EventInterface::Other("FocusEvent");
    if let Some(old) = old {
        fire(
            browser,
            doc,
            old,
            "blur",
            focus_iface,
            false,
            false,
            Vec::new(),
        );
        fire(
            browser,
            doc,
            old,
            "focusout",
            focus_iface,
            true,
            false,
            Vec::new(),
        );
    }
    fire(
        browser,
        doc,
        target,
        "focus",
        focus_iface,
        false,
        false,
        Vec::new(),
    );
    fire(
        browser,
        doc,
        target,
        "focusin",
        focus_iface,
        true,
        false,
        Vec::new(),
    );
}

// ----- keys --------------------------------------------------------------------

/// The `key` of a raw key value (WebDriver §15.2.1, the table of
/// normalized key values) and its `location`.
pub fn key_of(c: char) -> (String, u32) {
    let named = match c {
        '\u{E000}' => "Unidentified",
        '\u{E001}' => "Cancel",
        '\u{E002}' => "Help",
        '\u{E003}' => "Backspace",
        '\u{E004}' => "Tab",
        '\u{E005}' => "Clear",
        '\u{E006}' | '\u{E007}' => "Enter",
        '\u{E008}' => "Shift",
        '\u{E009}' => "Control",
        '\u{E00A}' => "Alt",
        '\u{E00B}' => "Pause",
        '\u{E00C}' => "Escape",
        '\u{E00D}' => " ",
        '\u{E00E}' | '\u{E054}' => "PageUp",
        '\u{E00F}' | '\u{E055}' => "PageDown",
        '\u{E010}' | '\u{E056}' => "End",
        '\u{E011}' | '\u{E057}' => "Home",
        '\u{E012}' | '\u{E058}' => "ArrowLeft",
        '\u{E013}' | '\u{E059}' => "ArrowUp",
        '\u{E014}' | '\u{E05A}' => "ArrowRight",
        '\u{E015}' | '\u{E05B}' => "ArrowDown",
        '\u{E016}' | '\u{E05C}' => "Insert",
        '\u{E017}' | '\u{E05D}' => "Delete",
        '\u{E018}' => ";",
        '\u{E019}' => "=",
        '\u{E024}' => "*",
        '\u{E025}' => "+",
        '\u{E026}' => ",",
        '\u{E027}' => "-",
        '\u{E028}' => ".",
        '\u{E029}' => "/",
        '\u{E03D}' | '\u{E053}' => "Meta",
        '\u{E040}' => "ZenkakuHankaku",
        '\u{E050}' => "Shift",
        '\u{E051}' => "Control",
        '\u{E052}' => "Alt",
        _ => "",
    };
    let location = match c {
        '\u{E050}'..='\u{E053}' => 2,
        '\u{E01A}'..='\u{E029}' => 3,
        '\u{E008}'..='\u{E00A}' | '\u{E03D}' => 1,
        _ => 0,
    };
    if !named.is_empty() {
        return (named.to_string(), location);
    }
    match c {
        '\u{E01A}'..='\u{E023}' => (
            char::from(b'0' + (c as u32 - 0xE01A) as u8).to_string(),
            location,
        ),
        '\u{E031}'..='\u{E03C}' => (format!("F{}", c as u32 - 0xE030), 0),
        '\u{E000}'..='\u{F8FF}' => ("Unidentified".to_string(), 0),
        c => (c.to_string(), 0),
    }
}

/// The `code` of a raw key value (US layout, the common keys).
pub fn code_of(c: char) -> String {
    match c {
        'a'..='z' | 'A'..='Z' => format!("Key{}", c.to_ascii_uppercase()),
        '0'..='9' => format!("Digit{c}"),
        ' ' | '\u{E00D}' => "Space".to_string(),
        '\u{E003}' => "Backspace".to_string(),
        '\u{E004}' => "Tab".to_string(),
        '\u{E006}' | '\u{E007}' => "Enter".to_string(),
        '\u{E008}' => "ShiftLeft".to_string(),
        '\u{E009}' => "ControlLeft".to_string(),
        '\u{E00A}' => "AltLeft".to_string(),
        '\u{E03D}' => "MetaLeft".to_string(),
        '\u{E050}' => "ShiftRight".to_string(),
        '\u{E051}' => "ControlRight".to_string(),
        '\u{E052}' => "AltRight".to_string(),
        '\u{E053}' => "MetaRight".to_string(),
        '\u{E00C}' => "Escape".to_string(),
        '\u{E012}' => "ArrowLeft".to_string(),
        '\u{E013}' => "ArrowUp".to_string(),
        '\u{E014}' => "ArrowRight".to_string(),
        '\u{E015}' => "ArrowDown".to_string(),
        '\u{E017}' => "Delete".to_string(),
        '-' => "Minus".to_string(),
        '=' => "Equal".to_string(),
        ',' => "Comma".to_string(),
        '.' => "Period".to_string(),
        '/' => "Slash".to_string(),
        ';' => "Semicolon".to_string(),
        _ => String::new(),
    }
}

fn set_modifier(m: &mut Modifiers, key: &str, down: bool) {
    match key {
        "Shift" => m.shift = down,
        "Control" => m.ctrl = down,
        "Alt" => m.alt = down,
        "Meta" => m.meta = down,
        _ => {}
    }
}

/// Whether `node` takes typed text: a `textarea`, or an `input` whose
/// type is a text field.
fn editable(d: &Document, node: NodeId) -> bool {
    if d.is_html_element(node, "textarea") {
        return true;
    }
    if !d.is_html_element(node, "input") {
        return false;
    }
    let t = d
        .get_attribute(node, "type")
        .unwrap_or("text")
        .to_ascii_lowercase();
    !matches!(
        t.as_str(),
        "checkbox"
            | "radio"
            | "button"
            | "submit"
            | "reset"
            | "image"
            | "file"
            | "hidden"
            | "color"
            | "range"
    )
}

/// The text value of an editable element: the `value` attribute of an
/// `input`, the text of a `textarea`.
fn text_value(d: &Document, node: NodeId) -> String {
    if d.is_html_element(node, "textarea") {
        d.text_content(node).unwrap_or_default()
    } else {
        d.get_attribute(node, "value").unwrap_or("").to_string()
    }
}

fn set_text_value(d: &mut Document, node: NodeId, value: &str) {
    if d.is_html_element(node, "textarea") {
        d.set_text_content(node, Some(value));
    } else {
        d.set_attribute(node, "value", value);
    }
}

/// Presses key `c` at `target`: `keydown`, then for a printable key
/// `keypress`, the text edit of an editable target and `input`.
fn key_down(browser: &mut Browser, input: &mut InputState, doc: &DocRef, target: NodeId, c: char) {
    let (key, location) = key_of(c);
    let repeat = input.pressed.contains(&c);
    if !repeat {
        input.pressed.push(c);
    }
    set_modifier(&mut input.modifiers, &key, true);
    let slots = |input: &InputState| {
        let mut s = vec![
            ("key", Slot::Str(key.clone())),
            ("code", Slot::Str(code_of(c))),
            ("location", Slot::Num(f64::from(location))),
            ("repeat", Slot::Bool(repeat)),
            ("isComposing", Slot::Bool(false)),
        ];
        s.extend(modifier_slots(input.modifiers));
        s
    };
    let iface = EventInterface::Other("KeyboardEvent");
    if !fire(
        browser,
        doc,
        target,
        "keydown",
        iface,
        true,
        true,
        slots(&*input),
    ) {
        return;
    }
    let m = input.modifiers;
    let printable = key.chars().count() == 1 && !m.ctrl && !m.meta;
    let enter = key == "Enter";
    if (printable || enter)
        && !fire(
            browser,
            doc,
            target,
            "keypress",
            iface,
            true,
            true,
            slots(&*input),
        )
    {
        return;
    }
    let is_editable = editable(&doc.borrow(), target);
    if !is_editable {
        return;
    }
    let edited = {
        let mut d = doc.borrow_mut();
        let mut value = text_value(&d, target);
        if printable {
            value.push_str(&key);
            set_text_value(&mut d, target, &value);
            true
        } else if key == "Backspace" && !value.is_empty() {
            value.pop();
            set_text_value(&mut d, target, &value);
            true
        } else {
            false
        }
    };
    if edited {
        fire(
            browser,
            doc,
            target,
            "input",
            EventInterface::Other("InputEvent"),
            true,
            false,
            Vec::new(),
        );
    }
}

/// Releases key `c` at `target`: `keyup`.
fn key_up(browser: &mut Browser, input: &mut InputState, doc: &DocRef, target: NodeId, c: char) {
    let Some(i) = input.pressed.iter().position(|x| *x == c) else {
        return;
    };
    input.pressed.remove(i);
    let (key, location) = key_of(c);
    set_modifier(&mut input.modifiers, &key, false);
    let mut s = vec![
        ("key", Slot::Str(key)),
        ("code", Slot::Str(code_of(c))),
        ("location", Slot::Num(f64::from(location))),
        ("repeat", Slot::Bool(false)),
        ("isComposing", Slot::Bool(false)),
    ];
    s.extend(modifier_slots(input.modifiers));
    fire(
        browser,
        doc,
        target,
        "keyup",
        EventInterface::Other("KeyboardEvent"),
        true,
        true,
        s,
    );
}

// ----- action sequences ------------------------------------------------------------

/// One action of a source (WebDriver §15.4.2).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Pause(u64),
    KeyDown(char),
    KeyUp(char),
    PointerDown(u16),
    PointerUp(u16),
    PointerMove {
        x: f64,
        y: f64,
        duration: u64,
        origin: Origin,
    },
    PointerCancel,
    Scroll {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
        duration: u64,
        origin: Origin,
    },
}

/// The origin of a pointer move or scroll.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Viewport,
    Pointer,
    Element(String),
}

fn duration_of(v: &Json) -> WdResult<u64> {
    match v.get("duration") {
        None | Some(Json::Null) => Ok(0),
        Some(Json::Number(n)) if *n >= 0.0 && n.fract() == 0.0 => Ok(*n as u64),
        _ => invalid_argument("duration must be a non-negative integer"),
    }
}

fn number_of(v: &Json, name: &str, default: Option<f64>) -> WdResult<f64> {
    match v.get(name) {
        Some(Json::Number(n)) => Ok(*n),
        None | Some(Json::Null) if default.is_some() => Ok(default.unwrap_or(0.0)),
        _ => invalid_argument(&format!("{name} must be a number")),
    }
}

fn origin_of(v: &Json) -> WdResult<Origin> {
    match v.get("origin") {
        None | Some(Json::Null) => Ok(Origin::Viewport),
        Some(Json::String(s)) if s == "viewport" => Ok(Origin::Viewport),
        Some(Json::String(s)) if s == "pointer" => Ok(Origin::Pointer),
        Some(o @ Json::Object(_)) => match o.get(script::ELEMENT_KEY).and_then(Json::as_str) {
            Some(id) => Ok(Origin::Element(id.to_string())),
            None => invalid_argument("origin must be viewport, pointer or an element"),
        },
        _ => invalid_argument("origin must be viewport, pointer or an element"),
    }
}

fn key_value(v: &Json) -> WdResult<char> {
    let Some(s) = v.get("value").and_then(Json::as_str) else {
        return invalid_argument("value must be a string");
    };
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => invalid_argument("value must be a single key"),
    }
}

fn button_of(v: &Json) -> WdResult<u16> {
    match v.get("button") {
        Some(Json::Number(n)) if *n >= 0.0 && *n <= 15.0 && n.fract() == 0.0 => Ok(*n as u16),
        _ => invalid_argument("button must be a small non-negative integer"),
    }
}

/// "Extract an action sequence": the actions of each source, in order.
pub fn parse_actions(body: &Json) -> WdResult<Vec<Vec<Action>>> {
    let Some(sources) = body.get("actions").and_then(Json::as_array) else {
        return invalid_argument("actions must be an array");
    };
    let mut out = Vec::new();
    for source in sources {
        let Some(kind) = source.get("type").and_then(Json::as_str) else {
            return invalid_argument("an input source needs a type");
        };
        if !matches!(kind, "none" | "key" | "pointer" | "wheel") {
            return invalid_argument(&format!("invalid input source type {kind}"));
        }
        if source.get("id").and_then(Json::as_str).is_none() {
            return invalid_argument("an input source needs an id");
        }
        if kind == "pointer" {
            if let Some(p) = source.get("parameters") {
                match p.get("pointerType").and_then(Json::as_str) {
                    None | Some("mouse") | Some("pen") | Some("touch") => {}
                    Some(_) => return invalid_argument("invalid pointerType"),
                }
            }
        }
        let Some(items) = source.get("actions").and_then(Json::as_array) else {
            return invalid_argument("actions of a source must be an array");
        };
        let mut actions = Vec::new();
        for item in items {
            if item.as_object().is_none() {
                return invalid_argument("an action must be an object");
            }
            let Some(t) = item.get("type").and_then(Json::as_str) else {
                return invalid_argument("an action needs a type");
            };
            let a = match (kind, t) {
                (_, "pause") => Action::Pause(duration_of(item)?),
                ("key", "keyDown") => Action::KeyDown(key_value(item)?),
                ("key", "keyUp") => Action::KeyUp(key_value(item)?),
                ("pointer", "pointerDown") => Action::PointerDown(button_of(item)?),
                ("pointer", "pointerUp") => Action::PointerUp(button_of(item)?),
                ("pointer", "pointerMove") => Action::PointerMove {
                    x: number_of(item, "x", Some(0.0))?,
                    y: number_of(item, "y", Some(0.0))?,
                    duration: duration_of(item)?,
                    origin: origin_of(item)?,
                },
                ("pointer", "pointerCancel") => Action::PointerCancel,
                ("wheel", "scroll") => Action::Scroll {
                    x: number_of(item, "x", None)?,
                    y: number_of(item, "y", None)?,
                    delta_x: number_of(item, "deltaX", None)?,
                    delta_y: number_of(item, "deltaY", None)?,
                    duration: duration_of(item)?,
                    origin: origin_of(item)?,
                },
                _ => return invalid_argument(&format!("invalid action {t} for a {kind} source")),
            };
            actions.push(a);
        }
        out.push(actions);
    }
    Ok(out)
}

/// The target and position of a move or scroll.
fn resolve_origin(
    session: &Session,
    ctx: ContextId,
    doc: &DocRef,
    origin: &Origin,
    x: f64,
    y: f64,
) -> WdResult<(Option<NodeId>, f64, f64)> {
    let (target, x, y) = match origin {
        Origin::Viewport => (None, x, y),
        Origin::Pointer => (None, session.input.x + x, session.input.y + y),
        Origin::Element(id) => {
            let (_, node) = script::known_element(&session.elements, ctx, id)?;
            (Some(node), x, y)
        }
    };
    let (w, h) = (f64::from(VIEWPORT_WIDTH), f64::from(VIEWPORT_HEIGHT));
    let inside = (0.0..=w).contains(&x) && (0.0..=h).contains(&y);
    if target.is_none() && !inside {
        return Err(WdError::new(
            ErrorCode::MoveTargetOutOfBounds,
            format!("({x}, {y}) is outside the viewport"),
        ));
    }
    let _ = doc;
    Ok((target, x, y))
}

/// Runs the event loop for `ms` milliseconds of real time.
fn wait(browser: &mut Browser, ms: u64) {
    if ms == 0 {
        return;
    }
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        let now = Instant::now();
        if now >= end {
            return;
        }
        let left = end - now;
        browser.run_until(left, |_| false);
        std::thread::sleep(left.min(Duration::from_millis(2)));
    }
}

/// "Perform actions" in the current browsing context `ctx`: tick by
/// tick, each tick runs the next action of every source and lasts as
/// long as its longest duration.
pub fn perform_actions(
    browser: &mut Browser,
    session: &mut Session,
    ctx: ContextId,
    body: &Json,
) -> WdResult<()> {
    let sources = parse_actions(body)?;
    let ticks = sources.iter().map(Vec::len).max().unwrap_or(0);
    for tick in 0..ticks {
        let mut duration = 0;
        for source in &sources {
            let Some(action) = source.get(tick) else {
                continue;
            };
            let doc = active_document(ctx)?;
            match action {
                Action::Pause(d) => duration = duration.max(*d),
                Action::KeyDown(c) => {
                    if let Some(t) = key_target(&session.input, &doc) {
                        key_down(browser, &mut session.input, &doc, t, *c);
                    }
                }
                Action::KeyUp(c) => {
                    if let Some(t) = key_target(&session.input, &doc) {
                        key_up(browser, &mut session.input, &doc, t, *c);
                    }
                }
                Action::PointerDown(b) => pointer_down(browser, &mut session.input, &doc, *b),
                Action::PointerUp(b) => pointer_up(browser, &mut session.input, &doc, *b),
                Action::PointerMove {
                    x,
                    y,
                    duration: d,
                    origin,
                } => {
                    duration = duration.max(*d);
                    let (target, x, y) = resolve_origin(session, ctx, &doc, origin, *x, *y)?;
                    let target = target.or_else(|| pointer_target(&session.input, &doc));
                    if let Some(t) = target {
                        move_pointer(browser, &mut session.input, &doc, t, x, y);
                    }
                }
                Action::PointerCancel => {
                    if let Some(t) = pointer_target(&session.input, &doc) {
                        fire(
                            browser,
                            &doc,
                            t,
                            "pointercancel",
                            POINTER,
                            true,
                            false,
                            pointer_slots(&session.input, -1),
                        );
                    }
                    session.input.buttons = 0;
                }
                Action::Scroll {
                    x,
                    y,
                    delta_x,
                    delta_y,
                    duration: d,
                    origin,
                } => {
                    duration = duration.max(*d);
                    let (target, x, y) = resolve_origin(session, ctx, &doc, origin, *x, *y)?;
                    let target = target.or_else(|| pointer_target(&session.input, &doc));
                    if let Some(t) = target {
                        let mut s = vec![
                            ("clientX", Slot::Num(x)),
                            ("clientY", Slot::Num(y)),
                            ("deltaX", Slot::Num(*delta_x)),
                            ("deltaY", Slot::Num(*delta_y)),
                            ("deltaZ", Slot::Num(0.0)),
                            ("deltaMode", Slot::Num(0.0)),
                        ];
                        s.extend(modifier_slots(session.input.modifiers));
                        fire(
                            browser,
                            &doc,
                            t,
                            "wheel",
                            EventInterface::Other("WheelEvent"),
                            true,
                            true,
                            s,
                        );
                    }
                }
            }
        }
        wait(browser, duration);
    }
    Ok(())
}

/// "Release actions": every pressed key goes up (last first), every
/// pressed button too; the input state is reset.
pub fn release_actions(browser: &mut Browser, session: &mut Session, ctx: ContextId) {
    if let Ok(doc) = active_document(ctx) {
        let keys: Vec<char> = session.input.pressed.iter().rev().copied().collect();
        for c in keys {
            if let Some(t) = key_target(&session.input, &doc) {
                key_up(browser, &mut session.input, &doc, t, c);
            }
        }
        for b in 0..16u16 {
            if session.input.buttons & (1 << b) != 0 {
                pointer_up(browser, &mut session.input, &doc, b);
            }
        }
    }
    let focus = session.input.focus.take();
    session.input = InputState {
        focus,
        ..InputState::default()
    };
}

// ----- element interaction -------------------------------------------------------

/// "Element Click": the pointer moves to the element and button 0 goes
/// down and up on it.
pub fn element_click(
    browser: &mut Browser,
    session: &mut Session,
    ctx: ContextId,
    id: &str,
) -> WdResult<()> {
    let (doc, node) = script::known_element(&session.elements, ctx, id)?;
    let is_file = {
        let d = doc.borrow();
        d.is_html_element(node, "input")
            && d.get_attribute(node, "type")
                .is_some_and(|t| t.eq_ignore_ascii_case("file"))
    };
    if is_file {
        return invalid_argument("cannot click a file input");
    }
    let (x, y) = (session.input.x, session.input.y);
    move_pointer(browser, &mut session.input, &doc, node, x, y);
    pointer_down(browser, &mut session.input, &doc, 0);
    pointer_up(browser, &mut session.input, &doc, 0);
    Ok(())
}

/// "Element Send Keys": focuses the element and types `text`; the
/// modifier keys in it stay down until the end (or a `U+E000` null key).
pub fn element_send_keys(
    browser: &mut Browser,
    session: &mut Session,
    ctx: ContextId,
    id: &str,
    text: &str,
) -> WdResult<()> {
    let (doc, node) = script::known_element(&session.elements, ctx, id)?;
    focus(browser, &mut session.input, &doc, node);
    let mut held: Vec<char> = Vec::new();
    for c in text.chars() {
        let (key, _) = key_of(c);
        let modifier = matches!(key.as_str(), "Shift" | "Control" | "Alt" | "Meta");
        if c == '\u{E000}' {
            for m in held.drain(..).rev() {
                key_up(browser, &mut session.input, &doc, node, m);
            }
            continue;
        }
        if modifier {
            if held.contains(&c) {
                held.retain(|x| *x != c);
                key_up(browser, &mut session.input, &doc, node, c);
            } else {
                held.push(c);
                key_down(browser, &mut session.input, &doc, node, c);
            }
            continue;
        }
        key_down(browser, &mut session.input, &doc, node, c);
        key_up(browser, &mut session.input, &doc, node, c);
    }
    for m in held.into_iter().rev() {
        key_up(browser, &mut session.input, &doc, node, m);
    }
    Ok(())
}

// ----- the window's input ----------------------------------------------------------

/// The pointer of the window moved to the client point (`x`, `y`): it
/// targets the element hit testing finds there.
pub fn window_pointer_move(
    browser: &mut Browser,
    input: &mut InputState,
    doc: &DocRef,
    x: f64,
    y: f64,
) {
    let target = crate::view::element_from_point(doc, x, y).or_else(|| default_target(doc));
    if let Some(t) = target {
        move_pointer(browser, input, doc, t, x, y);
    }
}

/// Mouse button `button` (0 main, 1 auxiliary, 2 secondary) of the
/// window went down or up at the pointer.
pub fn window_button(
    browser: &mut Browser,
    input: &mut InputState,
    doc: &DocRef,
    button: u16,
    down: bool,
) {
    if down {
        pointer_down(browser, input, doc, button);
    } else {
        pointer_up(browser, input, doc, button);
    }
}

/// Key `c` (a raw key value) of the window went down or up at the
/// focused element (or the body).
pub fn window_key(
    browser: &mut Browser,
    input: &mut InputState,
    doc: &DocRef,
    c: char,
    down: bool,
) {
    let Some(t) = key_target(input, doc) else {
        return;
    };
    if down {
        key_down(browser, input, doc, t, c);
    } else {
        key_up(browser, input, doc, t, c);
    }
}

/// The wheel of the window turned by (`dx`, `dy`) pixels over the
/// pointer: a `wheel` event at the element under it. Returns that
/// element if the event was not canceled (the window then scrolls).
pub fn window_wheel(
    browser: &mut Browser,
    input: &mut InputState,
    doc: &DocRef,
    dx: f64,
    dy: f64,
) -> Option<NodeId> {
    let (x, y) = (input.x, input.y);
    let target = crate::view::element_from_point(doc, x, y).or_else(|| default_target(doc))?;
    let mut s = vec![
        ("clientX", Slot::Num(x)),
        ("clientY", Slot::Num(y)),
        ("deltaX", Slot::Num(dx)),
        ("deltaY", Slot::Num(dy)),
        ("deltaZ", Slot::Num(0.0)),
        ("deltaMode", Slot::Num(0.0)),
    ];
    s.extend(modifier_slots(input.modifiers));
    let iface = EventInterface::Other("WheelEvent");
    fire(browser, doc, target, "wheel", iface, true, true, s).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_key_values() {
        assert_eq!(key_of('a'), ("a".to_string(), 0));
        assert_eq!(key_of('\u{E007}'), ("Enter".to_string(), 0));
        assert_eq!(key_of('\u{E008}'), ("Shift".to_string(), 1));
        assert_eq!(key_of('\u{E050}'), ("Shift".to_string(), 2));
        assert_eq!(key_of('\u{E01D}'), ("3".to_string(), 3));
        assert_eq!(key_of('\u{E031}'), ("F1".to_string(), 0));
        assert_eq!(code_of('q'), "KeyQ");
        assert_eq!(code_of('7'), "Digit7");
    }

    #[test]
    fn parses_an_action_sequence() {
        let body = Json::parse(
            r#"{"actions":[
                {"type":"pointer","id":"m","parameters":{"pointerType":"mouse"},
                 "actions":[{"type":"pointerMove","x":10,"y":20},{"type":"pointerDown","button":0},{"type":"pointerUp","button":0}]},
                {"type":"key","id":"k","actions":[{"type":"keyDown","value":""},{"type":"pause","duration":5}]},
                {"type":"none","id":"n","actions":[{"type":"pause"}]}]}"#,
        )
        .unwrap();
        let a = parse_actions(&body).unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(
            a[0][0],
            Action::PointerMove {
                x: 10.0,
                y: 20.0,
                duration: 0,
                origin: Origin::Viewport
            }
        );
        assert_eq!(a[0][1], Action::PointerDown(0));
        assert_eq!(a[1], vec![Action::KeyDown('\u{E008}'), Action::Pause(5)]);
        assert_eq!(a[2], vec![Action::Pause(0)]);
    }

    #[test]
    fn rejects_invalid_actions() {
        for bad in [
            r#"{}"#,
            r#"{"actions":[{"type":"key","actions":[]}]}"#,
            r#"{"actions":[{"type":"key","id":"k","actions":[{"type":"keyDown","value":"ab"}]}]}"#,
            r#"{"actions":[{"type":"key","id":"k","actions":[{"type":"pointerDown","button":0}]}]}"#,
            r#"{"actions":[{"type":"pointer","id":"p","actions":[{"type":"pause","duration":-1}]}]}"#,
            r#"{"actions":[{"type":"glove","id":"g","actions":[{"type":"wave"}]}]}"#,
        ] {
            let body = Json::parse(bad).unwrap();
            assert_eq!(
                parse_actions(&body).err().map(|e| e.code),
                Some(ErrorCode::InvalidArgument),
                "{bad}"
            );
        }
    }
}
