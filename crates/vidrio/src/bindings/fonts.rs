//! CSS Font Loading 3 over `vgfx::font` and [`crate::css::font_face`]:
//! `FontFace` (from a `src` list or from bytes), `FontFaceSet` as
//! `document.fonts` with its CSS-connected faces (one per `@font-face`
//! rule of the document's sheets), `add`, `delete`, `clear`, `load`,
//! `check`, `status`, `ready` and the `loading`, `loadingdone` and
//! `loadingerror` events (`FontFaceSetLoadEvent`).
//!
//! The faces of a document live on the Rust side ([`SetData`], one per
//! document), so that font selection ([`web_faces`]) works without
//! script; the JavaScript objects are made when script asks for them.
//! CSS-connected faces start loading when they are discovered
//! ([`update_document`], from "the end" of parsing and from each
//! rendering update), not when text first uses them (LIMITATIONS.md);
//! their loads delay the document's `load` event, as style sheets do.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use vgfx::font::matching::{Descriptor, Request, StyleRange, best_faces};
use vgfx::font::system::{FontIndex, WebFace};
use vgfx::font::{Face, decode_container};
use vjs::gc::Realm;
use vjs::object::ObjectKind;
use vjs::{Agent, JsResult, Object, Throw, Value};

use super::RealmState;
use super::exception;
use super::platform::{self, Native};
use super::value::{IdlValue, Invocation};
use crate::browser::event_loop::queue_task;
use crate::browser::{TaskSource, in_realm};
use crate::css::font_face::{self as ff, FontSource};
use crate::css::media::Environment;
use crate::css::om::{self, Rule, RuleKind, RuleRef};
use crate::css::sheets;
use crate::dom::events::{Event, EventInit, EventInterface, Target, dispatch};
use crate::dom::{DocRef, Document};
use crate::fetch::{self, Kept};
use crate::url::Url;

type R = JsResult<IdlValue>;

/// The attributes of `FontFace`, their `@font-face` descriptors and
/// their default values (CSS Font Loading 3 §2.1).
const ATTRIBUTES: [(&str, &str, &str); 11] = [
    ("family", "font-family", ""),
    ("style", "font-style", "normal"),
    ("weight", "font-weight", "normal"),
    ("stretch", "font-stretch", "normal"),
    ("unicodeRange", "unicode-range", "U+0-10FFFF"),
    ("featureSettings", "font-feature-settings", "normal"),
    ("variationSettings", "font-variation-settings", "normal"),
    ("display", "font-display", "auto"),
    ("ascentOverride", "ascent-override", "normal"),
    ("descentOverride", "descent-override", "normal"),
    ("lineGapOverride", "line-gap-override", "normal"),
];

/// How many `@import` levels the search for `@font-face` rules follows.
const MAX_IMPORT_DEPTH: usize = 16;

fn attribute_index(name: &str) -> Option<usize> {
    ATTRIBUTES.iter().position(|a| a.0 == name)
}

/// Whether `value` parses as attribute `i` (the family is a plain
/// string).
fn valid(i: usize, value: &str) -> bool {
    match i {
        0 => true,
        1 => ff::parse_style(value).is_some(),
        2 => ff::parse_weight(value).is_some(),
        3 => ff::parse_stretch(value).is_some(),
        4 => ff::parse_unicode_range(value).is_some(),
        5 => ff::parse_feature_settings(value).is_some(),
        6 => ff::valid_variation_settings(value),
        7 => ff::valid_display(value),
        _ => ff::valid_metric_override(value),
    }
}

fn defaults() -> [String; 11] {
    ATTRIBUTES.map(|a| a.2.to_string())
}

/// `FontFace.status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Unloaded,
    Loading,
    Loaded,
    Error,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unloaded => "unloaded",
            Status::Loading => "loading",
            Status::Loaded => "loaded",
            Status::Error => "error",
        }
    }
}

// ----- faces ------------------------------------------------------------------------

/// The state of a `FontFace`.
pub struct FaceData {
    values: RefCell<[String; 11]>,
    status: Cell<Status>,
    /// The `DOMException` name the promise rejects with on error.
    error: Cell<&'static str>,
    /// `src` of a face made from a URL list.
    sources: RefCell<Vec<FontSource>>,
    /// The URL `url()` sources resolve against.
    base: Option<Url>,
    /// The document that fetches the sources.
    doc: Option<Weak<RefCell<Document>>>,
    face: RefCell<Option<Arc<Face>>>,
    /// The `@font-face` rule of a CSS-connected face.
    rule: Option<Weak<RefCell<Rule>>>,
    /// The JavaScript object, once made. It is alive while the face is
    /// in a set (the set's backing `Set` holds it) or [`FaceData::keep`]
    /// holds it; only those paths read it.
    object: Cell<Option<Object>>,
    /// Roots the object while a load runs.
    keep: RefCell<Option<Kept>>,
    /// The document whose `load` event a load delays.
    delaying: RefCell<Option<DocRef>>,
    /// Run when the load settles (true: loaded).
    waiters: RefCell<Vec<Box<dyn FnOnce(&mut Agent, bool)>>>,
    /// The sets the face was added to.
    sets: RefCell<Vec<Weak<SetData>>>,
}

impl FaceData {
    fn new(
        values: [String; 11],
        sources: Vec<FontSource>,
        base: Option<Url>,
        doc: Option<&DocRef>,
        rule: Option<&RuleRef>,
    ) -> Rc<FaceData> {
        Rc::new(FaceData {
            values: RefCell::new(values),
            status: Cell::new(Status::Unloaded),
            error: Cell::new("NetworkError"),
            sources: RefCell::new(sources),
            base,
            doc: doc.map(Rc::downgrade),
            face: RefCell::new(None),
            rule: rule.map(Rc::downgrade),
            object: Cell::new(None),
            keep: RefCell::new(None),
            delaying: RefCell::new(None),
            waiters: RefCell::new(Vec::new()),
            sets: RefCell::new(Vec::new()),
        })
    }

    pub fn status(&self) -> Status {
        self.status.get()
    }

    pub fn is_css_connected(&self) -> bool {
        self.rule.is_some()
    }

    /// The family name faces are matched by.
    pub fn family(&self) -> String {
        let v = &self.values.borrow()[0];
        ff::parse_family_name(v).unwrap_or_else(|| v.trim().to_string())
    }

    /// The matching descriptor (the initial values if one is invalid).
    pub fn descriptor(&self) -> Descriptor {
        let v = self.values.borrow();
        ff::descriptor(&v[2], &v[3], &v[1]).unwrap_or(Descriptor {
            weight: (400.0, 400.0),
            stretch: (100.0, 100.0),
            style: StyleRange::Normal,
        })
    }

    pub fn unicode_range(&self) -> Vec<(u32, u32)> {
        ff::parse_unicode_range(&self.values.borrow()[4]).unwrap_or_default()
    }

    fn covers(&self, cp: u32) -> bool {
        let r = self.unicode_range();
        r.is_empty() || r.iter().any(|&(a, b)| cp >= a && cp <= b)
    }

    /// The loaded face for font selection.
    pub fn web_face(&self) -> Option<WebFace> {
        let face = self.face.borrow().clone()?;
        Some(WebFace {
            family: self.family(),
            descriptor: self.descriptor(),
            unicode_range: self.unicode_range(),
            face,
        })
    }
}

fn mark_handled(agent: &mut Agent, promise: Value) {
    if let Value::Object(p) = promise {
        if let ObjectKind::Promise(d) = &mut agent.heap.object_mut(p).kind {
            d.is_handled = true;
        }
    }
}

/// Gives the object of `face` its `[[FontStatusPromise]]`, settled if the
/// face already is.
fn init_promise(agent: &mut Agent, realm: Realm, o: Object, face: &FaceData) {
    let Some(p) = platform::platform_ref(agent, Value::Object(o)) else {
        return;
    };
    let cap = vjs::interp::promise::new_intrinsic_capability(agent, realm);
    p.get().set_slot("[[FontStatusPromise]]", cap.promise);
    p.get().set_slot("[[resolve]]", cap.resolve);
    p.get().set_slot("[[reject]]", cap.reject);
    match face.status.get() {
        Status::Loaded => settle_promise(agent, o, true, ""),
        Status::Error => settle_promise(agent, o, false, face.error.get()),
        _ => {}
    }
}

/// Fulfills the `[[FontStatusPromise]]` of `o` with `o`, or rejects it
/// with a `DOMException` named `error` (marked as handled, so that a
/// face nobody waits for does not report an unhandled rejection).
fn settle_promise(agent: &mut Agent, o: Object, ok: bool, error: &str) {
    let Some(p) = platform::platform_ref(agent, Value::Object(o)) else {
        return;
    };
    let realm = p.get().realm;
    let promise = p
        .get()
        .slot("[[FontStatusPromise]]")
        .unwrap_or(Value::Undefined);
    let resolve = p.get().slot("[[resolve]]").unwrap_or(Value::Undefined);
    let reject = p.get().slot("[[reject]]").unwrap_or(Value::Undefined);
    if resolve.is_nullish() {
        return;
    }
    p.get().set_slot("[[resolve]]", Value::Undefined);
    p.get().set_slot("[[reject]]", Value::Undefined);
    in_realm(agent, realm, |agent| {
        if ok {
            let _ = vjs::ops::call(agent, resolve, Value::Undefined, &[Value::Object(o)]);
        } else {
            mark_handled(agent, promise);
            let e = Value::Object(exception::create(agent, error, ""));
            let _ = vjs::ops::call(agent, reject, Value::Undefined, &[e]);
        }
    });
}

/// The object of `face` in the realm of `state` (made the first time).
fn face_object(
    agent: &mut Agent,
    state: &RealmState,
    face: &Rc<FaceData>,
    proto: Option<Object>,
) -> Object {
    if let Some(o) = face.object.get() {
        return o;
    }
    let o = fetch::create_object(agent, state, "FontFace", Native::Data(face.clone()), proto);
    face.object.set(Some(o));
    init_promise(agent, state.realm, o, face);
    if face.status.get() == Status::Loading {
        *face.keep.borrow_mut() = Some(fetch::keep(vec![Value::Object(o)]));
    }
    o
}

/// Changes the status of `face` and tells the sets it is in.
fn set_status(agent: &mut Agent, face: &Rc<FaceData>, status: Status) {
    if face.status.replace(status) == status {
        return;
    }
    let sets: Vec<Rc<SetData>> = face
        .sets
        .borrow()
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    for set in sets {
        if !set.contains(face) {
            continue;
        }
        match status {
            Status::Loading => set.face_loading(agent, face),
            Status::Loaded | Status::Error => {
                set.face_settled(agent, face, status == Status::Loaded)
            }
            Status::Unloaded => {}
        }
    }
}

fn parse_font(bytes: Vec<u8>) -> Option<Arc<Face>> {
    let data = decode_container(bytes).ok()?;
    Face::parse(data, 0).ok().map(Arc::new)
}

/// The load algorithm of a face made from a URL list (§2.2 `load()`):
/// the sources in order, the first that gives a font wins.
fn start_load(agent: &mut Agent, face: &Rc<FaceData>) {
    if face.status.get() != Status::Unloaded {
        return;
    }
    if let Some(o) = face.object.get() {
        *face.keep.borrow_mut() = Some(fetch::keep(vec![Value::Object(o)]));
    }
    let doc = face.doc.as_ref().and_then(Weak::upgrade);
    if let Some(doc) = &doc {
        if let Some(info) = crate::browser::ua().and_then(|ua| ua.doc_info(doc)) {
            info.load_delay.set(info.load_delay.get() + 1);
            *face.delaying.borrow_mut() = Some(doc.clone());
        }
    }
    set_status(agent, face, Status::Loading);
    let f = face.clone();
    queue_task(TaskSource::Networking, doc, move |agent| {
        try_source(agent, f, 0)
    });
}

fn try_source(agent: &mut Agent, face: Rc<FaceData>, i: usize) {
    let source = face.sources.borrow().get(i).cloned();
    match source {
        None => finish_load(agent, &face, None),
        Some(FontSource::Local(name)) => {
            let index = FontIndex::system();
            match index.local(&name).and_then(|k| index.load(k)) {
                Some(f) => finish_load(agent, &face, Some(f)),
                None => try_source(agent, face, i + 1),
            }
        }
        Some(FontSource::Url { url, .. }) => {
            let ua = crate::browser::ua();
            let url = Url::parse(&url, face.base.as_ref()).ok();
            let (Some(ua), Some(url)) = (ua, url) else {
                return try_source(agent, face, i + 1);
            };
            let doc = face.doc.as_ref().and_then(Weak::upgrade);
            crate::browser::loader::fetch(&ua, url, doc, move |agent, result| {
                let parsed = match result {
                    Ok(r) if (200..300).contains(&r.status) => parse_font(r.body),
                    _ => None,
                };
                match parsed {
                    Some(f) => finish_load(agent, &face, Some(f)),
                    None => try_source(agent, face, i + 1),
                }
            });
        }
    }
}

/// Settles a load: the face, its promise, the sets it is in, its
/// waiters, and the `load` event it delayed.
fn finish_load(agent: &mut Agent, face: &Rc<FaceData>, result: Option<Arc<Face>>) {
    let ok = result.is_some();
    *face.face.borrow_mut() = result;
    set_status(agent, face, if ok { Status::Loaded } else { Status::Error });
    if let Some(o) = face.object.get() {
        settle_promise(agent, o, ok, face.error.get());
    }
    let waiters = std::mem::take(&mut *face.waiters.borrow_mut());
    for w in waiters {
        w(agent, ok);
    }
    face.keep.borrow_mut().take();
    let delayed = face.delaying.borrow_mut().take();
    if let Some(doc) = delayed {
        if let Some(info) = crate::browser::ua().and_then(|ua| ua.doc_info(&doc)) {
            info.load_delay.set(info.load_delay.get().saturating_sub(1));
        }
        crate::browser::document::continue_the_end(agent, &doc);
    }
    fetch::settle(agent);
}

// ----- sets -------------------------------------------------------------------------

/// The `FontFaceSet` object of a set: its event target id, its realm and
/// itself (the realm's CSSOM identity map keeps it alive).
struct SetJs {
    target: u64,
    state: Weak<RealmState>,
    object: Object,
}

/// The font face set of a document (§2.3).
pub struct SetData {
    doc: Weak<RefCell<Document>>,
    /// CSS-connected faces in document order, with their rules.
    css: RefCell<Vec<(Weak<RefCell<Rule>>, Rc<FaceData>)>>,
    /// Faces added with `add()`, in insertion order.
    added: RefCell<Vec<Rc<FaceData>>>,
    /// `[[LoadingFonts]]`, `[[LoadedFonts]]`, `[[FailedFonts]]`.
    loading: RefCell<Vec<Rc<FaceData>>>,
    loaded: RefCell<Vec<Rc<FaceData>>>,
    failed: RefCell<Vec<Rc<FaceData>>>,
    /// The faces the backing `Set` of the object holds (weakly, so that
    /// an address is not reused while it is listed).
    synced: RefCell<Vec<Weak<FaceData>>>,
    /// Whether the set switched to loading since its last `loadingdone`.
    was_loading: Cell<bool>,
    finish_queued: Cell<bool>,
    js: RefCell<Option<SetJs>>,
}

thread_local! {
    /// The font face set of each document, by the address of its `RefCell`.
    static SETS: RefCell<HashMap<usize, Rc<SetData>>> = RefCell::new(HashMap::new());
}

fn doc_key(doc: &DocRef) -> usize {
    Rc::as_ptr(doc) as *const () as usize
}

/// The font face set of `doc` (made the first time).
pub fn set_of(doc: &DocRef) -> Rc<SetData> {
    let key = doc_key(doc);
    SETS.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(set) = s.get(&key) {
            if set.doc.upgrade().is_some_and(|d| Rc::ptr_eq(&d, doc)) {
                return set.clone();
            }
        }
        s.retain(|_, v| v.doc.strong_count() > 0);
        let set = Rc::new(SetData {
            doc: Rc::downgrade(doc),
            css: RefCell::new(Vec::new()),
            added: RefCell::new(Vec::new()),
            loading: RefCell::new(Vec::new()),
            loaded: RefCell::new(Vec::new()),
            failed: RefCell::new(Vec::new()),
            synced: RefCell::new(Vec::new()),
            was_loading: Cell::new(false),
            finish_queued: Cell::new(false),
            js: RefCell::new(None),
        });
        s.insert(key, set.clone());
        set
    })
}

/// The loaded faces of `doc`'s font face set, CSS-connected ones first,
/// for font selection (`vgfx::font::system::FontIndex::select_with`).
pub fn web_faces(doc: &DocRef) -> Vec<WebFace> {
    set_of(doc)
        .faces()
        .iter()
        .filter_map(|f| f.web_face())
        .collect()
}

/// Reads the `@font-face` rules of `doc` into its font face set and
/// starts loading the new ones; from "the end" of the document and from
/// each rendering update.
pub fn update_document(agent: &mut Agent, doc: &DocRef) {
    let set = set_of(doc);
    sync_css(&set, doc);
    sync_js(agent, &set);
    let faces: Vec<Rc<FaceData>> = set.css.borrow().iter().map(|e| e.1.clone()).collect();
    for f in faces {
        start_load(agent, &f);
    }
}

/// The document is no longer "pending on the environment" (its `load`
/// event fired): the set may switch to loaded.
pub fn environment_settled(agent: &mut Agent, doc: &DocRef) {
    update_document(agent, doc);
    set_of(doc).maybe_finish();
}

impl SetData {
    fn contains(&self, face: &Rc<FaceData>) -> bool {
        self.css.borrow().iter().any(|e| Rc::ptr_eq(&e.1, face))
            || self.added.borrow().iter().any(|f| Rc::ptr_eq(f, face))
    }

    /// The set entries: CSS-connected faces, then the added ones.
    pub fn faces(&self) -> Vec<Rc<FaceData>> {
        let mut v: Vec<Rc<FaceData>> = self.css.borrow().iter().map(|e| e.1.clone()).collect();
        v.extend(self.added.borrow().iter().cloned());
        v
    }

    fn js_state(&self) -> Option<(u64, Rc<RealmState>, Object)> {
        let js = self.js.borrow();
        let js = js.as_ref()?;
        Some((js.target, js.state.upgrade()?, js.object))
    }

    /// Whether the document still loads (§2.3 "pending on the
    /// environment").
    fn pending_on_environment(&self) -> bool {
        let Some(doc) = self.doc.upgrade() else {
            return false;
        };
        match crate::browser::ua().and_then(|ua| ua.doc_info(&doc)) {
            Some(info) => info.readiness.get() != crate::browser::document::Readiness::Complete,
            None => false,
        }
    }

    /// A face of the set started loading.
    fn face_loading(self: &Rc<Self>, agent: &mut Agent, face: &Rc<FaceData>) {
        let first = self.loading.borrow().is_empty();
        if !self.loading.borrow().iter().any(|f| Rc::ptr_eq(f, face)) {
            self.loading.borrow_mut().push(face.clone());
        }
        if !first {
            return;
        }
        // "Switch the FontFaceSet to loading".
        self.was_loading.set(true);
        let Some((_, state, o)) = self.js_state() else {
            return;
        };
        if let Some(p) = platform::platform_ref(agent, Value::Object(o)) {
            let resolved = p
                .get()
                .slot("[[readyResolve]]")
                .is_none_or(|v| v.is_nullish());
            if resolved {
                let cap = vjs::interp::promise::new_intrinsic_capability(agent, state.realm);
                p.get().set_slot("[[ReadyPromise]]", cap.promise);
                p.get().set_slot("[[readyResolve]]", cap.resolve);
            }
        }
        let set = self.clone();
        queue_task(
            TaskSource::DomManipulation,
            self.doc.upgrade(),
            move |agent| {
                set.fire(agent, "loading", &[]);
            },
        );
    }

    /// A face of the set finished loading.
    fn face_settled(self: &Rc<Self>, _agent: &mut Agent, face: &Rc<FaceData>, ok: bool) {
        self.loading.borrow_mut().retain(|f| !Rc::ptr_eq(f, face));
        let list = if ok { &self.loaded } else { &self.failed };
        list.borrow_mut().push(face.clone());
        self.maybe_finish();
    }

    /// Removes `face` from the loading lists (it left the set).
    fn forget(self: &Rc<Self>, face: &Rc<FaceData>) {
        for list in [&self.loading, &self.loaded, &self.failed] {
            list.borrow_mut().retain(|f| !Rc::ptr_eq(f, face));
        }
        self.maybe_finish();
    }

    /// "Switch the FontFaceSet to loaded" when nothing loads and the
    /// document is done: `loadingdone`, `loadingerror` and `ready`, in a
    /// task.
    fn maybe_finish(self: &Rc<Self>) {
        if !self.loading.borrow().is_empty()
            || self.pending_on_environment()
            || self.finish_queued.get()
        {
            return;
        }
        self.finish_queued.set(true);
        let set = self.clone();
        queue_task(
            TaskSource::DomManipulation,
            self.doc.upgrade(),
            move |agent| {
                set.finish_queued.set(false);
                if !set.loading.borrow().is_empty() {
                    return;
                }
                let loaded = std::mem::take(&mut *set.loaded.borrow_mut());
                let failed = std::mem::take(&mut *set.failed.borrow_mut());
                if set.was_loading.replace(false) {
                    set.fire(agent, "loadingdone", &loaded);
                    if !failed.is_empty() {
                        set.fire(agent, "loadingerror", &failed);
                    }
                }
                set.resolve_ready(agent);
            },
        );
    }

    fn resolve_ready(&self, agent: &mut Agent) {
        let Some((_, state, o)) = self.js_state() else {
            return;
        };
        let Some(p) = platform::platform_ref(agent, Value::Object(o)) else {
            return;
        };
        let resolve = p.get().slot("[[readyResolve]]").unwrap_or(Value::Undefined);
        if resolve.is_nullish() {
            return;
        }
        p.get().set_slot("[[readyResolve]]", Value::Undefined);
        in_realm(agent, state.realm, |agent| {
            let _ = vjs::ops::call(agent, resolve, Value::Undefined, &[Value::Object(o)]);
        });
        fetch::settle(agent);
    }

    /// Fires a `FontFaceSetLoadEvent` named `type_` whose `fontfaces` are
    /// those of `faces` still in the set.
    fn fire(&self, agent: &mut Agent, type_: &str, faces: &[Rc<FaceData>]) {
        let Some((target, state, _)) = self.js_state() else {
            return;
        };
        let Some(doc) = state.document() else {
            return;
        };
        let objects: Vec<Value> = faces
            .iter()
            .filter(|f| self.contains(f))
            .filter_map(|f| f.object.get())
            .map(Value::Object)
            .collect();
        let now = crate::browser::window::window_now(&state);
        let mut e = Event::new_trusted(type_, EventInit::default(), now);
        e.interface = EventInterface::Other("FontFaceSetLoadEvent");
        let ev = e.into_ref();
        let o = super::host::event_wrapper(agent, &state, &ev);
        let array = in_realm(agent, state.realm, |agent| {
            let a = vjs::ops::create_array_from_list(agent, &objects);
            let _ = vjs::ops::set_integrity_level(agent, a, vjs::ops::IntegrityLevel::Frozen);
            a
        });
        if let Some(p) = platform::platform_ref(agent, Value::Object(o)) {
            p.get().set_slot("fontfaces", Value::Object(array));
        }
        let mut host = super::host::JsHost::with_state(agent, doc.clone(), state.clone());
        dispatch(&doc, &mut host, &ev, Target::Other(target), false);
        drop(host);
        fetch::settle(agent);
    }
}

/// The `@font-face` rules of `rules` (and of imported sheets, matching
/// `@media` and true `@supports` blocks, and layers) with the base URL
/// of their sheet.
fn collect_font_faces(
    rules: &[RuleRef],
    base: &Option<Url>,
    env: &Environment,
    depth: usize,
    out: &mut Vec<(RuleRef, Option<Url>)>,
) {
    for rule in rules {
        let (children, child_base, child_depth) = {
            let r = rule.borrow();
            match &r.kind {
                RuleKind::FontFace { .. } => {
                    out.push((rule.clone(), base.clone()));
                    continue;
                }
                RuleKind::Import {
                    sheet: Some(s),
                    media,
                    ..
                } if depth < MAX_IMPORT_DEPTH && media.matches(env) => {
                    let (children, b) = sheet_rules(s);
                    (children, b.or_else(|| base.clone()), depth + 1)
                }
                RuleKind::Media { media, rules } if media.matches(env) => {
                    (rules.clone(), base.clone(), depth)
                }
                RuleKind::Supports {
                    matches: true,
                    rules,
                    ..
                }
                | RuleKind::LayerBlock { rules, .. } => (rules.clone(), base.clone(), depth),
                _ => continue,
            }
        };
        collect_font_faces(&children, &child_base, env, child_depth, out);
    }
}

fn sheet_rules(sheet: &om::SheetRef) -> (Vec<RuleRef>, Option<Url>) {
    let s = sheet.borrow();
    let base = s.base_url.as_deref().and_then(|b| Url::parse(b, None).ok());
    (s.rules.clone(), base)
}

/// The attribute values of a `@font-face` rule, and its `src`; `None`
/// if the rule defines no face (no valid `font-family` or `src`).
fn rule_face(rule: &Rule) -> Option<([String; 11], Vec<FontSource>)> {
    let style = rule.kind.style()?;
    let family = ff::parse_family_name(&style.property_value("font-family"))?;
    let src = ff::parse_src(&style.property_value("src"))?;
    let mut values = defaults();
    values[0] = family;
    for (i, (_, descriptor, _)) in ATTRIBUTES.iter().enumerate().skip(1) {
        let v = style.property_value(descriptor);
        if !v.is_empty() && valid(i, &v) {
            values[i] = v;
        }
    }
    Some((values, src))
}

/// Makes the CSS-connected faces of `set` follow the `@font-face` rules
/// of `doc`'s sheets: one face per rule, kept while the rule stays.
fn sync_css(set: &Rc<SetData>, doc: &DocRef) {
    let env = Environment::default();
    let doc_base = crate::browser::document::base_url(doc);
    let mut rules = Vec::new();
    for sheet in sheets::applicable_sheets(doc, &env) {
        let (children, base) = sheet_rules(&sheet);
        let base = base.or_else(|| Some(doc_base.clone()));
        collect_font_faces(&children, &base, &env, 0, &mut rules);
    }
    let old = std::mem::take(&mut *set.css.borrow_mut());
    let mut new = Vec::new();
    for (rule, base) in rules {
        let Some((values, src)) = rule_face(&rule.borrow()) else {
            continue;
        };
        let existing = old
            .iter()
            .find(|(w, _)| w.upgrade().is_some_and(|r| Rc::ptr_eq(&r, &rule)));
        if let Some((_, face)) = existing {
            *face.values.borrow_mut() = values;
            if face.status.get() == Status::Unloaded {
                *face.sources.borrow_mut() = src;
            }
            new.push((Rc::downgrade(&rule), face.clone()));
            continue;
        }
        let face = FaceData::new(values, src, base, Some(doc), Some(&rule));
        face.sets.borrow_mut().push(Rc::downgrade(set));
        new.push((Rc::downgrade(&rule), face));
    }
    *set.css.borrow_mut() = new;
    for (_, face) in old {
        if !set.contains(&face) {
            set.forget(&face);
        }
    }
}

/// Gives every face of `set` an object in the set's realm and makes the
/// set's backing `Set` hold exactly its entries, in order.
fn sync_js(agent: &mut Agent, set: &Rc<SetData>) {
    let Some((_, state, o)) = set.js_state() else {
        return;
    };
    let Some(p) = platform::platform_ref(agent, Value::Object(o)) else {
        return;
    };
    let faces = set.faces();
    let same = {
        let synced = set.synced.borrow();
        synced.len() == faces.len()
            && synced
                .iter()
                .zip(&faces)
                .all(|(w, f)| std::ptr::eq(w.as_ptr(), Rc::as_ptr(f)))
    };
    if same {
        return;
    }
    *set.synced.borrow_mut() = faces.iter().map(Rc::downgrade).collect();
    let objects: Vec<Object> = faces
        .iter()
        .map(|f| face_object(agent, &state, f, None))
        .collect();
    let _ = in_realm(agent, state.realm, |agent| -> JsResult<()> {
        let backing = super::install::backing_collection(agent, &p, false)?;
        let clear = agent.heap.key_from_str("clear");
        vjs::ops::invoke(agent, Value::Object(backing), clear, &[])?;
        let add = agent.heap.key_from_str("add");
        for f in objects {
            vjs::ops::invoke(agent, Value::Object(backing), add, &[Value::Object(f)])?;
        }
        Ok(())
    });
}

/// The `FontFaceSet` object of `doc` in the realm of `state`.
fn fonts_object(agent: &mut Agent, state: &Rc<RealmState>, doc: &DocRef) -> Object {
    let set = set_of(doc);
    let key = (doc_key(doc), b'F');
    let cached = state.cssom.borrow().get(&key).map(|e| e.1);
    if let Some(o) = cached {
        update_document(agent, doc);
        return o;
    }
    let target = fetch::new_target_id(state);
    let o = fetch::create_object(
        agent,
        state,
        "FontFaceSet",
        Native::TargetData(target, set.clone()),
        None,
    );
    state.targets.borrow_mut().insert(target, o);
    state
        .cssom
        .borrow_mut()
        .insert(key, (set.clone() as Rc<dyn Any>, o));
    if let Some(p) = platform::platform_ref(agent, Value::Object(o)) {
        let cap = vjs::interp::promise::new_intrinsic_capability(agent, state.realm);
        p.get().set_slot("[[ReadyPromise]]", cap.promise);
        p.get().set_slot("[[readyResolve]]", cap.resolve);
    }
    *set.js.borrow_mut() = Some(SetJs {
        target,
        state: Rc::downgrade(state),
        object: o,
    });
    update_document(agent, doc);
    if set.loading.borrow().is_empty() && !set.pending_on_environment() {
        set.resolve_ready(agent);
    }
    o
}

/// "Find the matching font faces" (§2.4): `None` if `font` is not a
/// valid `font` value.
fn find_matching(set: &SetData, font: &str, text: &str) -> Option<Vec<Rc<FaceData>>> {
    let q = ff::parse_font_shorthand(font)?;
    let req = Request {
        weight: q.weight,
        stretch: q.stretch,
        style: q.style,
    };
    let chars: Vec<u32> = text.chars().map(u32::from).collect();
    let faces = set.faces();
    let mut out: Vec<Rc<FaceData>> = Vec::new();
    for family in &q.families {
        let ids: Vec<usize> = (0..faces.len())
            .filter(|&i| faces[i].family().eq_ignore_ascii_case(family))
            .collect();
        if ids.is_empty() {
            continue;
        }
        let descs: Vec<Descriptor> = ids.iter().map(|&i| faces[i].descriptor()).collect();
        for k in best_faces(&descs, &req) {
            let f = &faces[ids[k]];
            let used = chars.is_empty() || chars.iter().any(|&c| f.covers(c));
            if used && !out.iter().any(|g| Rc::ptr_eq(g, f)) {
                out.push(f.clone());
            }
        }
    }
    Some(out)
}

// ----- the bindings' entry point ----------------------------------------------------

fn throw_named<T>(agent: &mut Agent, name: &str) -> JsResult<T> {
    let o = exception::create(agent, name, "");
    Err(Throw(Value::Object(o)))
}

fn this_face(agent: &mut Agent, inv: &Invocation) -> JsResult<Rc<FaceData>> {
    fetch::this_data::<FaceData>(agent, inv)
}

fn this_set(agent: &mut Agent, inv: &Invocation) -> JsResult<Rc<SetData>> {
    fetch::this_data::<SetData>(agent, inv)
}

/// Runs `inv` if it is a member of CSS Font Loading.
pub fn call(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> Option<R> {
    let name = inv.op.name();
    let (owner, member) = name.split_once('_')?;
    if member.starts_with("get_on") || member.starts_with("set_on") {
        return None;
    }
    Some(match owner {
        "FontFaceSource" if member == "get_fonts" => {
            let doc = inv.this.as_ref().and_then(|p| {
                let native = p.get().native.borrow();
                native.document().cloned()
            });
            let Some(doc) = doc else {
                return Some(agent.type_error("Illegal invocation"));
            };
            Ok(IdlValue::Object(fonts_object(agent, state, &doc)))
        }
        "FontFace" => font_face(agent, state, inv, member),
        "FontFaceSet" => font_face_set(agent, state, inv, member),
        "FontFaceSetLoadEvent" if member == "get_fontfaces" => {
            let this = inv.this.as_ref()?;
            let v = this.get().slot("fontfaces");
            let array = match v {
                Some(Value::Object(a)) => a,
                _ => vjs::ops::create_array_from_list(agent, &[]),
            };
            if let Err(e) =
                vjs::ops::set_integrity_level(agent, array, vjs::ops::IntegrityLevel::Frozen)
            {
                return Some(Err(e));
            }
            this.get().set_slot("fontfaces", Value::Object(array));
            Ok(IdlValue::Object(array))
        }
        "FontFacePalettes" | "FontFacePalette" => match member {
            "get_length" => Ok(IdlValue::UInt(0)),
            "get_usableWithLightBackground" | "get_usableWithDarkBackground" => {
                Ok(IdlValue::Bool(false))
            }
            _ => return None,
        },
        _ => return None,
    })
}

fn font_face(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    if member == "constructor" {
        return construct_face(agent, state, inv);
    }
    let face = this_face(agent, inv)?;
    let this = inv.this.clone().expect("a FontFace");
    if let Some(attr) = member.strip_prefix("get_") {
        if let Some(i) = attribute_index(attr) {
            return Ok(IdlValue::String(face.values.borrow()[i].clone()));
        }
    }
    if let Some(attr) = member.strip_prefix("set_") {
        if let Some(i) = attribute_index(attr) {
            let value = inv.str(0);
            if !valid(i, &value) {
                return throw_named(agent, "SyntaxError");
            }
            face.values.borrow_mut()[i] = value.clone();
            // A CSS-connected face changes its rule too (§2.1).
            if let Some(rule) = face.rule.as_ref().and_then(Weak::upgrade) {
                if let Some(style) = rule.borrow_mut().kind.style_mut() {
                    style.set_property(ATTRIBUTES[i].1, &value, "");
                }
                om::sheets_changed();
            }
            return Ok(IdlValue::Undefined);
        }
    }
    match member {
        "get_status" => Ok(IdlValue::String(face.status.get().as_str().into())),
        "get_loaded" => Ok(IdlValue::Any(
            this.get()
                .slot("[[FontStatusPromise]]")
                .unwrap_or(Value::Undefined),
        )),
        "load" => {
            start_load(agent, &face);
            Ok(IdlValue::Any(
                this.get()
                    .slot("[[FontStatusPromise]]")
                    .unwrap_or(Value::Undefined),
            ))
        }
        "get_features" | "get_variations" | "get_palettes" => {
            let (slot, iface) = match member {
                "get_features" => ("[[features]]", "FontFaceFeatures"),
                "get_variations" => ("[[variations]]", "FontFaceVariations"),
                _ => ("[[palettes]]", "FontFacePalettes"),
            };
            if let Some(Value::Object(o)) = this.get().slot(slot) {
                return Ok(IdlValue::Object(o));
            }
            let o = fetch::create_object(agent, state, iface, Native::None, None);
            this.get().set_slot(slot, Value::Object(o));
            Ok(IdlValue::Object(o))
        }
        _ => agent.type_error(&format!("FontFace.{member} is not supported")),
    }
}

/// `new FontFace(family, source, descriptors)` (§2.1).
fn construct_face(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let mut values = defaults();
    values[0] = inv.str(0);
    let mut ok = true;
    if let Some(d) = inv.arg(2).as_dict() {
        for (i, (attr, _, _)) in ATTRIBUTES.iter().enumerate().skip(1) {
            if let Some(v) = d.str(attr) {
                ok &= valid(i, v);
                values[i] = v.to_string();
            }
        }
    }
    let doc = state.document();
    let base = doc.as_ref().map(crate::browser::document::base_url);
    let source = inv.arg(1);
    let bytes = source
        .as_object()
        .and_then(|o| fetch::buffer_source_bytes(agent, o));
    let sources = match (&bytes, source.as_object()) {
        (None, None) => match ff::parse_src(source.as_str()) {
            Some(s) => s,
            None => {
                ok = false;
                Vec::new()
            }
        },
        _ => Vec::new(),
    };
    let face = FaceData::new(values, sources, base, doc.as_ref(), None);
    if !ok {
        face.error.set("SyntaxError");
        face.status.set(Status::Error);
    } else if bytes.is_some() {
        face.error.set("SyntaxError");
        face.status.set(Status::Loading);
    }
    let proto = super::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
    let o = face_object(agent, state, &face, proto);
    if let (true, Some(bytes)) = (ok, bytes) {
        // The bytes are parsed "in parallel"; the result lands in a task.
        let f = face.clone();
        queue_task(TaskSource::Networking, doc, move |agent| {
            let parsed = parse_font(bytes);
            finish_load(agent, &f, parsed);
        });
    }
    Ok(IdlValue::Object(o))
}

fn font_face_set(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    let set = this_set(agent, inv)?;
    let this = inv.this.clone().expect("a FontFaceSet");
    if let Some(doc) = set.doc.upgrade() {
        update_document(agent, &doc);
    }
    match member {
        "add" => {
            let face = match inv
                .object(0)
                .and_then(|o| fetch::object_data::<FaceData>(agent, o))
            {
                Some(f) => f,
                None => return agent.type_error("The value is not of type 'FontFace'."),
            };
            if !set.contains(&face) {
                if face.is_css_connected() {
                    return throw_named(agent, "InvalidModificationError");
                }
                if face.object.get().is_none() {
                    face.object.set(inv.object(0));
                }
                set.added.borrow_mut().push(face.clone());
                face.sets.borrow_mut().push(Rc::downgrade(&set));
                if face.status.get() == Status::Loading {
                    set.face_loading(agent, &face);
                }
                sync_js(agent, &set);
            }
            Ok(IdlValue::Object(this.object))
        }
        "delete" => {
            let Some(face) = inv
                .object(0)
                .and_then(|o| fetch::object_data::<FaceData>(agent, o))
            else {
                return Ok(IdlValue::Bool(false));
            };
            if face.is_css_connected() {
                return Ok(IdlValue::Bool(false));
            }
            let before = set.added.borrow().len();
            set.added.borrow_mut().retain(|f| !Rc::ptr_eq(f, &face));
            let deleted = set.added.borrow().len() != before;
            if deleted {
                set.forget(&face);
                sync_js(agent, &set);
            }
            Ok(IdlValue::Bool(deleted))
        }
        "clear" => {
            let gone = std::mem::take(&mut *set.added.borrow_mut());
            for f in &gone {
                set.forget(f);
            }
            sync_js(agent, &set);
            Ok(IdlValue::Undefined)
        }
        "get_status" => Ok(IdlValue::String(
            if set.loading.borrow().is_empty() {
                "loaded"
            } else {
                "loading"
            }
            .into(),
        )),
        "get_ready" => Ok(IdlValue::Any(
            this.get()
                .slot("[[ReadyPromise]]")
                .unwrap_or(Value::Undefined),
        )),
        "check" => match find_matching(&set, &inv.str(0), &inv.opt_str(1).unwrap_or(" ".into())) {
            None => throw_named(agent, "SyntaxError"),
            Some(found) => Ok(IdlValue::Bool(
                found.iter().all(|f| f.status.get() == Status::Loaded),
            )),
        },
        "load" => load_matching(agent, state, &set, inv),
        _ => agent.type_error(&format!("FontFaceSet.{member} is not supported")),
    }
}

/// What `FontFaceSet.load()` waits for.
struct Waiting {
    deferred: fetch::Deferred,
    faces: Vec<Object>,
    left: Cell<usize>,
    failed: Cell<bool>,
    realm: Realm,
    _keep: Kept,
}

impl Waiting {
    fn done(&self, agent: &mut Agent, ok: bool) {
        if !ok {
            self.failed.set(true);
        }
        let left = self.left.get().saturating_sub(1);
        self.left.set(left);
        if left == 0 {
            self.settle(agent);
        }
    }

    fn settle(&self, agent: &mut Agent) {
        in_realm(agent, self.realm, |agent| {
            if self.failed.get() {
                let e = Value::Object(exception::create(agent, "NetworkError", ""));
                self.deferred.reject(agent, e);
            } else {
                let list: Vec<Value> = self.faces.iter().map(|o| Value::Object(*o)).collect();
                let a = vjs::ops::create_array_from_list(agent, &list);
                self.deferred.resolve(agent, Value::Object(a));
            }
        });
    }
}

/// `FontFaceSet.load(font, text)` (§2.5): loads the matching faces and
/// fulfills with them once all have loaded.
fn load_matching(agent: &mut Agent, state: &Rc<RealmState>, set: &SetData, inv: &Invocation) -> R {
    let realm = state.realm;
    let text = inv.opt_str(1).unwrap_or(" ".into());
    let Some(found) = find_matching(set, &inv.str(0), &text) else {
        let e = fetch::dom_exception_value(agent, realm, "SyntaxError", "");
        return Ok(IdlValue::Object(fetch::rejected(agent, realm, e)));
    };
    let deferred = fetch::Deferred::new(agent, realm);
    let promise = deferred.promise;
    let faces: Vec<Object> = found
        .iter()
        .map(|f| face_object(agent, state, f, None))
        .collect();
    let mut kept = deferred.values();
    kept.extend(faces.iter().map(|o| Value::Object(*o)));
    let waiting = Rc::new(Waiting {
        deferred,
        faces,
        left: Cell::new(found.len()),
        failed: Cell::new(false),
        realm,
        _keep: fetch::keep(kept),
    });
    if found.is_empty() {
        waiting.settle(agent);
    }
    for f in &found {
        start_load(agent, f);
        match f.status.get() {
            Status::Loaded => waiting.done(agent, true),
            Status::Error => waiting.done(agent, false),
            _ => {
                let w = waiting.clone();
                f.waiters
                    .borrow_mut()
                    .push(Box::new(move |agent: &mut Agent, ok: bool| {
                        w.done(agent, ok)
                    }));
            }
        }
    }
    Ok(IdlValue::Object(promise))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_validation() {
        assert_eq!(attribute_index("unicodeRange"), Some(4));
        assert_eq!(attribute_index("nope"), None);
        assert!(valid(0, "anything at all"));
        assert!(valid(2, "bold"));
        assert!(!valid(2, "heavy"));
        assert!(valid(4, "U+0-7F, U+100"));
        assert!(!valid(4, "U+110000"));
        assert!(valid(7, "swap"));
        assert!(!valid(8, "-5%"));
        assert_eq!(Status::Loading.as_str(), "loading");
    }

    #[test]
    fn faces_match_by_family_style_and_range() {
        let doc: DocRef = Rc::new(RefCell::new(Document::new_html()));
        let set = set_of(&doc);
        let mk = |family: &str, weight: &str, range: &str| {
            let mut v = defaults();
            v[0] = family.into();
            v[2] = weight.into();
            v[4] = range.into();
            FaceData::new(v, Vec::new(), None, None, None)
        };
        let regular = mk("Ahem", "normal", "U+0-10FFFF");
        let bold = mk("Ahem", "bold", "U+0-10FFFF");
        let greek = mk("Other", "normal", "U+370-3FF");
        set.added
            .borrow_mut()
            .extend([regular.clone(), bold.clone(), greek.clone()]);
        let found = find_matching(&set, "bold 10px Ahem", " ").unwrap();
        assert_eq!(found.len(), 1);
        assert!(Rc::ptr_eq(&found[0], &bold));
        let found = find_matching(&set, "10px Other", "a").unwrap();
        assert!(found.is_empty());
        let found = find_matching(&set, "10px Other", "\u{3b1}").unwrap();
        assert_eq!(found.len(), 1);
        assert!(find_matching(&set, "Ahem", " ").is_none());
        assert!(Rc::ptr_eq(&set_of(&doc), &set));
        assert!(web_faces(&doc).is_empty());
    }
}
