//! Browsing contexts (HTML §7.3) with their session history (§7.4), the
//! creation of `Window` objects and documents, navigation (§7.4.2),
//! unloading and discarding (§7.4.6), `iframe` elements (§4.8.5) and
//! auxiliary browsing contexts (`window.open`, §7.2.2.1).
//!
//! Each browsing context has one `WindowProxy` for its whole life and a
//! new `Window` (a new realm) for every document it displays. The session
//! history is kept per browsing context: entries made by `pushState` and
//! fragment navigations share their document; going back or forward to
//! an entry of another document loads its URL again (there is no
//! back/forward cache, LIMITATIONS.md).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vjs::gc::{Realm, Trace, Tracer};
use vjs::{Agent, Object, Value};

use super::clone::Serialized;
use super::document::{DocInfo, Readiness};
use super::event_loop::{TaskSource, queue_task};
use super::{UserAgent, doc_key};
use crate::bindings::{RealmState, WindowOptions};
use crate::dom::events::{Event, EventInit, EventInterface, Target, dispatch};
use crate::dom::{DocRef, Document, NodeId};
use crate::url::{Origin, Url};

pub type ContextId = u32;

/// The active `Window` of a browsing context.
#[derive(Clone)]
pub struct ActiveWindow {
    pub realm: Realm,
    pub state: Rc<RealmState>,
    pub doc: DocRef,
}

/// A session history entry.
pub struct HistoryEntry {
    pub url: Url,
    /// The serialized state of `pushState`/`replaceState`.
    pub state: Option<Rc<Serialized>>,
    /// The document the entry was made for (`None` once discarded).
    pub document: Option<Weak<RefCell<Document>>>,
}

#[derive(Default)]
pub struct SessionHistory {
    pub entries: Vec<HistoryEntry>,
    pub index: usize,
}

/// A browsing context (a navigable and its active browsing context).
pub struct BrowsingContext {
    pub id: ContextId,
    pub parent: Option<ContextId>,
    /// The container (`iframe`) element in the parent's document.
    pub container: RefCell<Option<(Weak<RefCell<Document>>, NodeId)>>,
    pub opener: Cell<Option<ContextId>>,
    pub name: RefCell<String>,
    pub proxy: Object,
    pub window: RefCell<Option<ActiveWindow>>,
    pub session: RefCell<SessionHistory>,
    /// The ongoing navigation (0 for none); a response of an older one is
    /// ignored.
    pub navigation: Cell<u64>,
    /// Created by `window.open`: scripts may close it.
    pub script_closable: Cell<bool>,
    /// `close()` was called.
    pub closing: Cell<bool>,
    pub discarded: Cell<bool>,
}

impl Trace for BrowsingContext {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.object(self.proxy);
        for e in self.session.borrow().entries.iter() {
            if let Some(s) = &e.state {
                s.trace(tracer);
            }
        }
    }
}

impl BrowsingContext {
    pub fn active_window(&self) -> Option<ActiveWindow> {
        self.window.borrow().clone()
    }

    pub fn active_document(&self) -> Option<DocRef> {
        self.window.borrow().as_ref().map(|w| w.doc.clone())
    }

    /// The document of the container element.
    pub fn container_document(&self) -> Option<DocRef> {
        self.container
            .borrow()
            .as_ref()
            .and_then(|(d, _)| d.upgrade())
    }

    pub fn container_element(&self) -> Option<(DocRef, NodeId)> {
        let c = self.container.borrow();
        let (d, n) = c.as_ref()?;
        Some((d.upgrade()?, *n))
    }
}

static NEXT_NAVIGATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn new_navigation_id() -> u64 {
    NEXT_NAVIGATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The children of context `id` whose container is in its active
/// document, in tree order of the containers.
pub fn child_contexts(ua: &UserAgent, id: ContextId) -> Vec<Rc<BrowsingContext>> {
    let Some(doc) = ua.context(id).and_then(|c| c.active_document()) else {
        return Vec::new();
    };
    let mut children: Vec<(NodeId, Rc<BrowsingContext>)> = ua
        .contexts
        .borrow()
        .iter()
        .filter(|c| c.parent == Some(id) && !c.discarded.get())
        .filter_map(|c| {
            let (d, n) = c.container_element()?;
            Rc::ptr_eq(&d, &doc).then(|| (n, c.clone()))
        })
        .collect();
    let d = doc.borrow();
    let order = d.inclusive_descendants(NodeId::DOCUMENT);
    children.retain(|(n, _)| d.is_connected(*n));
    children.sort_by_key(|(n, _)| order.iter().position(|x| x == n).unwrap_or(usize::MAX));
    children.into_iter().map(|(_, c)| c).collect()
}

/// The active documents of every fully active browsing context.
pub fn fully_active_documents(ua: &UserAgent) -> Vec<DocRef> {
    let contexts: Vec<Rc<BrowsingContext>> = ua.contexts.borrow().clone();
    contexts
        .iter()
        .filter(|c| !c.discarded.get())
        .filter_map(|c| c.active_document())
        .filter(|d| ua.is_fully_active(d))
        .collect()
}

// ----- creation ------------------------------------------------------------

fn new_context(
    agent: &mut Agent,
    ua: &UserAgent,
    parent: Option<ContextId>,
    container: Option<(DocRef, NodeId)>,
) -> Rc<BrowsingContext> {
    let id = ua.new_context_id();
    let proxy = super::window::create_proxy(agent, id);
    let ctx = Rc::new(BrowsingContext {
        id,
        parent,
        container: RefCell::new(container.map(|(d, n)| (Rc::downgrade(&d), n))),
        opener: Cell::new(None),
        name: RefCell::new(String::new()),
        proxy,
        window: RefCell::new(None),
        session: RefCell::new(SessionHistory::default()),
        navigation: Cell::new(0),
        script_closable: Cell::new(false),
        closing: Cell::new(false),
        discarded: Cell::new(false),
    });
    ua.contexts.borrow_mut().push(ctx.clone());
    ctx
}

/// Creates a `Window` for `doc` in context `ctx` and makes it active.
/// The document must already have its URL, origin and type.
pub fn install_document(
    agent: &mut Agent,
    ua: &UserAgent,
    ctx: &BrowsingContext,
    doc: DocRef,
) -> ActiveWindow {
    let secure = {
        let d = doc.borrow();
        matches!(d.url.scheme.as_str(), "https" | "file" | "about")
            || d.url.host.as_ref().is_some_and(|h| {
                let h = h.to_string();
                h == "localhost" || h == "127.0.0.1" || h.ends_with(".localhost")
            })
    };
    let realm = crate::bindings::create_window_realm_with(
        agent,
        doc.clone(),
        WindowOptions {
            secure_context: secure,
        },
        Some(ctx.proxy),
        Some(ctx.id),
    );
    let state = crate::bindings::state_of(agent, realm).expect("a window realm has a state");
    state.html.time_origin.set(ua.event_loop.clock.now());
    let info = Rc::new(DocInfo::new(&doc));
    info.context.set(Some(ctx.id));
    info.realm.set(Some(realm));
    ua.docs.borrow_mut().insert(doc_key(&doc), info);
    let w = ActiveWindow { realm, state, doc };
    *ctx.window.borrow_mut() = Some(w.clone());
    w
}

/// The origin a new `about:blank` or `about:srcdoc` document inherits.
fn inherited_origin(ua: &UserAgent, ctx: &BrowsingContext) -> Origin {
    if let Some(d) = ctx.container_document() {
        return d.borrow().origin.clone();
    }
    if let Some(o) = ctx
        .opener
        .get()
        .and_then(|o| ua.context(o))
        .and_then(|c| c.active_document())
    {
        return o.borrow().origin.clone();
    }
    Origin::new_opaque()
}

/// The initial `about:blank` document of a new browsing context: `html`,
/// `head` and `body`, completely loaded.
fn install_initial_about_blank(agent: &mut Agent, ua: &UserAgent, ctx: &BrowsingContext) {
    let mut d = Document::new_html();
    d.url = Url::parse("about:blank", None).expect("about:blank parses");
    d.origin = inherited_origin(ua, ctx);
    crate::html::parse_into(&mut d, "", true);
    let doc: DocRef = Rc::new(RefCell::new(d));
    let w = install_document(agent, ua, ctx, doc.clone());
    if let Some(info) = ua.doc_info(&w.doc) {
        info.is_initial_about_blank.set(true);
        info.readiness.set(Readiness::Complete);
        info.completely_loaded.set(true);
    }
    let url = doc.borrow().url.clone();
    let mut s = ctx.session.borrow_mut();
    s.entries = vec![HistoryEntry {
        url,
        state: None,
        document: Some(Rc::downgrade(&doc)),
    }];
    s.index = 0;
}

/// Creates a top-level browsing context (with `opener`, for an auxiliary
/// one) showing its initial `about:blank` document.
pub fn create_top_level(agent: &mut Agent, opener: Option<ContextId>, name: &str) -> ContextId {
    let ua = super::ua().expect("a user agent");
    let ctx = new_context(agent, &ua, None, None);
    ctx.opener.set(opener);
    *ctx.name.borrow_mut() = name.to_string();
    install_initial_about_blank(agent, &ua, &ctx);
    ctx.id
}

/// Creates the child browsing context of an `iframe` that was connected.
pub fn create_child(agent: &mut Agent, parent_doc: &DocRef, element: NodeId) -> Option<ContextId> {
    let ua = super::ua()?;
    let parent = ua.doc_info(parent_doc)?.context.get()?;
    let ctx = new_context(
        agent,
        &ua,
        Some(parent),
        Some((parent_doc.clone(), element)),
    );
    if let Some(name) = parent_doc.borrow().get_attribute(element, "name") {
        *ctx.name.borrow_mut() = name.to_string();
    }
    install_initial_about_blank(agent, &ua, &ctx);
    Some(ctx.id)
}

/// The child browsing context of an `iframe` element.
pub fn context_of_container(
    ua: &UserAgent,
    doc: &DocRef,
    element: NodeId,
) -> Option<Rc<BrowsingContext>> {
    ua.contexts
        .borrow()
        .iter()
        .find(|c| {
            !c.discarded.get()
                && c.container.borrow().as_ref().is_some_and(|(d, n)| {
                    *n == element && d.upgrade().is_some_and(|d| Rc::ptr_eq(&d, doc))
                })
        })
        .cloned()
}

// ----- events --------------------------------------------------------------

/// Fires a trusted event of `interface` named `type_` at the window of
/// `state`; `slots` become attributes of the event's wrapper. With
/// `legacy_target_override` the event's target is the document (as for
/// `load`). Returns false if the event was canceled.
pub fn fire_window_event(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    type_: &str,
    interface: EventInterface,
    init: EventInit,
    slots: &[(&'static str, Value)],
    legacy_target_override: bool,
) -> bool {
    let Some(doc) = state.document() else {
        return true;
    };
    let now = super::window::window_now(state);
    let mut e = Event::new_trusted(type_, init, now);
    e.interface = interface;
    let ev = e.into_ref();
    let o = crate::bindings::host::event_wrapper(agent, state, &ev);
    if let Some(p) = crate::bindings::platform::platform_ref(agent, Value::Object(o)) {
        for (k, v) in slots {
            p.get().set_slot(*k, *v);
        }
    }
    let mut host = crate::bindings::host::JsHost::with_state(agent, doc.clone(), state.clone());
    dispatch(&doc, &mut host, &ev, Target::Window, legacy_target_override)
}

/// Fires a trusted `Event` named `type_` at `target` of `doc` (an element
/// or the document) in the window of `doc`.
pub fn fire_simple_event(
    agent: &mut Agent,
    doc: &DocRef,
    target: Target,
    type_: &str,
    init: EventInit,
) -> bool {
    let Some(ua) = super::ua() else {
        return true;
    };
    let Some(state) = super::window::state_of_document(agent, &ua, doc) else {
        return true;
    };
    let now = super::window::window_now(&state);
    let ev = Event::new_trusted(type_, init, now).into_ref();
    let mut host = crate::bindings::host::JsHost::with_state(agent, doc.clone(), state.clone());
    dispatch(doc, &mut host, &ev, target, false)
}

// ----- navigation ------------------------------------------------------------

/// The parameters of a navigation.
pub struct NavigateParams {
    pub url: Url,
    /// Replace the current session history entry instead of pushing one.
    pub replace: bool,
    /// The `srcdoc` of an iframe (the URL is `about:srcdoc`).
    pub srcdoc: Option<String>,
    /// The origin of the document that started the navigation.
    pub source_origin: Option<Origin>,
    /// The method, headers and body when the navigation does not `GET`
    /// (a form submitted as an entity body).
    pub request: Option<super::loader::NetRequest>,
}

impl NavigateParams {
    pub fn new(url: Url) -> NavigateParams {
        NavigateParams {
            url,
            replace: false,
            srcdoc: None,
            source_origin: None,
            request: None,
        }
    }
}

/// True if `a` and `b` differ only in their fragment.
fn equal_except_fragment(a: &Url, b: &Url) -> bool {
    a.serialize(true) == b.serialize(true)
}

/// Navigates context `id` (HTML §7.4.2.2, simplified): `javascript:` URLs
/// run in the current window, fragment changes stay in the document, and
/// anything else is fetched and replaces the document when its response
/// arrives.
pub fn navigate(agent: &mut Agent, id: ContextId, params: NavigateParams) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(ctx) = ua.context(id) else {
        return;
    };
    let url = params.url.clone();
    if url.scheme == "javascript" {
        let encoded = url.serialize(false);
        let bytes = crate::url::percent::percent_decode_str(&encoded["javascript:".len()..]);
        let source = String::from_utf8_lossy(&bytes).into_owned();
        let Some(w) = ctx.active_window() else {
            return;
        };
        queue_task(TaskSource::Navigation, Some(w.doc.clone()), move |agent| {
            super::script::run_classic_source(agent, &w.state, &source, None);
        });
        return;
    }
    let current = ctx.active_document();
    let replace = params.replace
        || current
            .as_ref()
            .and_then(|d| ua.doc_info(d))
            .is_some_and(|i| i.is_initial_about_blank.get() || !i.completely_loaded.get());
    if params.srcdoc.is_none() && params.request.is_none() && url.fragment.is_some() {
        if let Some(doc) = &current {
            let same = equal_except_fragment(&doc.borrow().url, &url);
            if same {
                navigate_to_fragment(agent, &ua, &ctx, doc, url, params.replace);
                return;
            }
        }
    }
    let nav = new_navigation_id();
    ctx.navigation.set(nav);
    // "Prompt to unload": beforeunload, with no user to ask.
    if let Some(w) = ctx.active_window() {
        let init = EventInit {
            cancelable: true,
            ..EventInit::default()
        };
        fire_window_event(
            agent,
            &w.state,
            "beforeunload",
            EventInterface::Other("BeforeUnloadEvent"),
            init,
            &[],
            false,
        );
    }
    let request = super::loader::NavigationRequest {
        context: id,
        navigation: nav,
        url,
        replace,
        srcdoc: params.srcdoc,
        request: params.request,
    };
    super::loader::start_navigation(agent, &ua, request);
}

/// "Navigate to a fragment": same document, new URL and entry, then
/// `hashchange`.
fn navigate_to_fragment(
    _agent: &mut Agent,
    ua: &UserAgent,
    ctx: &Rc<BrowsingContext>,
    doc: &DocRef,
    url: Url,
    replace: bool,
) {
    let old = doc.borrow().url.clone();
    doc.borrow_mut().url = url.clone();
    {
        let mut s = ctx.session.borrow_mut();
        let entry = HistoryEntry {
            url: url.clone(),
            state: None,
            document: Some(Rc::downgrade(doc)),
        };
        push_or_replace(&mut s, entry, replace);
    }
    if old.fragment == url.fragment {
        return;
    }
    let Some(w) = ctx.active_window() else {
        return;
    };
    let _ = ua;
    let old_url = old.href();
    let new_url = url.href();
    queue_task(
        TaskSource::DomManipulation,
        Some(doc.clone()),
        move |agent| {
            let o = Value::String(agent.heap.alloc_str(&old_url));
            let n = Value::String(agent.heap.alloc_str(&new_url));
            fire_window_event(
                agent,
                &w.state,
                "hashchange",
                EventInterface::Other("HashChangeEvent"),
                EventInit::default(),
                &[("oldURL", o), ("newURL", n)],
                false,
            );
        },
    );
}

/// Appends `entry` after the current one (dropping the forward entries),
/// or replaces the current one.
pub fn push_or_replace(s: &mut SessionHistory, entry: HistoryEntry, replace: bool) {
    if replace && !s.entries.is_empty() {
        let i = s.index;
        s.entries[i] = entry;
    } else {
        let keep = if s.entries.is_empty() { 0 } else { s.index + 1 };
        s.entries.truncate(keep);
        s.entries.push(entry);
        s.index = s.entries.len() - 1;
    }
}

/// Replaces the document of `ctx` by `doc` (made by a navigation): the
/// old one is unloaded and discarded, the new one gets a `Window` and a
/// session history entry.
pub fn replace_document(
    agent: &mut Agent,
    ua: &UserAgent,
    ctx: &Rc<BrowsingContext>,
    doc: DocRef,
    replace: bool,
) -> ActiveWindow {
    if let Some(old) = ctx.active_window() {
        unload_document(agent, ua, &old);
        discard_window(agent, ua, ctx, &old);
    }
    let w = install_document(agent, ua, ctx, doc.clone());
    let url = doc.borrow().url.clone();
    let mut s = ctx.session.borrow_mut();
    push_or_replace(
        &mut s,
        HistoryEntry {
            url,
            state: None,
            document: Some(Rc::downgrade(&doc)),
        },
        replace,
    );
    w
}

/// "Unload a document": `pagehide` and `unload` at its window.
pub fn unload_document(agent: &mut Agent, ua: &UserAgent, w: &ActiveWindow) {
    let info = ua.doc_info(&w.doc);
    if let Some(i) = &info {
        if i.unloaded.get() {
            return;
        }
        i.unloaded.set(true);
        i.unload_counter.set(i.unload_counter.get() + 1);
    }
    fire_window_event(
        agent,
        &w.state,
        "pagehide",
        EventInterface::Other("PageTransitionEvent"),
        EventInit::default(),
        &[("persisted", Value::Boolean(false))],
        true,
    );
    fire_window_event(
        agent,
        &w.state,
        "unload",
        EventInterface::Event,
        EventInit::default(),
        &[],
        true,
    );
    super::event_loop::clean_up_after_running_script(agent);
    if let Some(i) = &info {
        i.unload_counter
            .set(i.unload_counter.get().saturating_sub(1));
    }
}

/// Discards the window `w` of `ctx`: its child contexts, timers, document
/// and realm.
fn discard_window(agent: &mut Agent, ua: &UserAgent, ctx: &BrowsingContext, w: &ActiveWindow) {
    let children: Vec<Rc<BrowsingContext>> = ua
        .contexts
        .borrow()
        .iter()
        .filter(|c| c.parent == Some(ctx.id) && !c.discarded.get())
        .cloned()
        .collect();
    for c in children {
        discard_context(agent, ua, &c);
    }
    if let Some(info) = ua.doc_info(&w.doc) {
        info.discarded.set(true);
        info.context.set(None);
    }
    super::event_loop::clear_timers_of(ua, w.realm);
    super::messaging::window_discarded(ua, w.realm);
    crate::bindings::discard_document(agent, w.realm);
    let mut slot = ctx.window.borrow_mut();
    if slot.as_ref().is_some_and(|x| x.realm == w.realm) {
        *slot = None;
    }
}

/// Destroys a browsing context: unloads and discards its document and
/// every descendant context. Its `WindowProxy` stays, with no window.
pub fn discard_context(agent: &mut Agent, ua: &UserAgent, ctx: &Rc<BrowsingContext>) {
    if ctx.discarded.get() {
        return;
    }
    ctx.navigation.set(0);
    if let Some(w) = ctx.active_window() {
        unload_document(agent, ua, &w);
        discard_window(agent, ua, ctx, &w);
    }
    ctx.discarded.set(true);
    for e in ctx.session.borrow_mut().entries.iter_mut() {
        e.state = None;
    }
}

/// `window.close()` on context `id` (if scripts may close it).
pub fn close(agent: &mut Agent, id: ContextId) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(ctx) = ua.context(id) else {
        return;
    };
    let closable = ctx.parent.is_none()
        && (ctx.script_closable.get() || ctx.session.borrow().entries.len() == 1);
    if !closable || ctx.closing.get() {
        return;
    }
    ctx.closing.set(true);
    let _ = agent;
    queue_task(TaskSource::Navigation, None, move |agent| {
        if let Some(ua) = super::ua() {
            if let Some(ctx) = ua.context(id) {
                discard_context(agent, &ua, &ctx);
            }
        }
    });
}

// ----- iframes ---------------------------------------------------------------

/// "Process the iframe attributes" of `element` in `doc`, whose child
/// context is `child`. `initial` is true on the first insertion.
pub fn process_iframe_attributes(
    agent: &mut Agent,
    doc: &DocRef,
    element: NodeId,
    child: ContextId,
    initial: bool,
) {
    let Some(ua) = super::ua() else {
        return;
    };
    let (srcdoc, src) = {
        let d = doc.borrow();
        (
            d.get_attribute(element, "srcdoc").map(str::to_string),
            d.get_attribute(element, "src").map(str::to_string),
        )
    };
    if let Some(text) = srcdoc {
        let url = Url::parse("about:srcdoc", None).expect("about:srcdoc parses");
        let mut p = NavigateParams::new(url);
        p.srcdoc = Some(text);
        p.replace = initial;
        mark_iframe_loading(&ua, doc, element);
        navigate(agent, child, p);
        return;
    }
    let url = src
        .filter(|s| !s.is_empty())
        .and_then(|s| Url::parse(&s, Some(&super::document::base_url(doc))).ok());
    match url {
        Some(url) if !(initial && url.href() == "about:blank") => {
            let mut p = NavigateParams::new(url);
            p.replace = initial;
            p.source_origin = Some(doc.borrow().origin.clone());
            mark_iframe_loading(&ua, doc, element);
            navigate(agent, child, p);
        }
        _ if initial => {
            // "Run the iframe load event steps" for the initial
            // about:blank document.
            fire_simple_event(
                agent,
                doc,
                Target::Node(element),
                "load",
                EventInit::default(),
            );
        }
        _ => {
            let url = Url::parse("about:blank", None).expect("about:blank parses");
            mark_iframe_loading(&ua, doc, element);
            navigate(agent, child, NavigateParams::new(url));
        }
    }
}

/// Records that the container document's load waits for an iframe.
fn mark_iframe_loading(ua: &UserAgent, doc: &DocRef, element: NodeId) {
    if let Some(info) = ua.doc_info(doc) {
        let mut loading = info.loading_iframes.borrow_mut();
        if !loading.contains(&element) {
            loading.push(element);
            info.load_delay.set(info.load_delay.get() + 1);
        }
    }
}

/// "Completely finish loading" of a child document: the iframe load
/// event steps at the container, then the parent's load may proceed.
pub fn child_finished_loading(agent: &mut Agent, ctx: &Rc<BrowsingContext>) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some((parent_doc, element)) = ctx.container_element() else {
        return;
    };
    fire_simple_event(
        agent,
        &parent_doc,
        Target::Node(element),
        "load",
        EventInit::default(),
    );
    super::event_loop::clean_up_after_running_script(agent);
    if let Some(info) = ua.doc_info(&parent_doc) {
        let was = {
            let mut loading = info.loading_iframes.borrow_mut();
            let before = loading.len();
            loading.retain(|n| *n != element);
            before != loading.len()
        };
        if was {
            info.load_delay.set(info.load_delay.get().saturating_sub(1));
            super::document::continue_the_end(agent, &parent_doc);
        }
    }
}

/// Keeps the iframes of `doc` in step with the tree: a connected iframe
/// without a context gets one; a context whose iframe left the document
/// is discarded.
pub fn reconcile_iframes(agent: &mut Agent, ua: &UserAgent, doc: &DocRef, info: &DocInfo) {
    let iframes: Vec<NodeId> = info.iframes.borrow().clone();
    for element in iframes {
        let connected = doc.borrow().is_connected(element);
        let existing = context_of_container(ua, doc, element);
        match (connected, existing) {
            (true, None) => {
                if !ua.scripting_enabled(doc) && info.context.get().is_none() {
                    continue;
                }
                if info.context.get().is_none() {
                    continue;
                }
                if let Some(child) = create_child(agent, doc, element) {
                    let (src, srcdoc) = {
                        let d = doc.borrow();
                        (
                            d.get_attribute(element, "src").map(str::to_string),
                            d.get_attribute(element, "srcdoc").map(str::to_string),
                        )
                    };
                    info.iframe_attrs
                        .borrow_mut()
                        .retain(|(n, _, _)| *n != element);
                    info.iframe_attrs.borrow_mut().push((element, src, srcdoc));
                    process_iframe_attributes(agent, doc, element, child, true);
                }
            }
            (false, Some(ctx)) => {
                discard_context(agent, ua, &ctx);
                let mut loading = info.loading_iframes.borrow_mut();
                let before = loading.len();
                loading.retain(|n| *n != element);
                if before != loading.len() {
                    info.load_delay.set(info.load_delay.get().saturating_sub(1));
                }
            }
            (true, Some(ctx)) => {
                // An attribute change navigates the child.
                let (src, srcdoc) = {
                    let d = doc.borrow();
                    (
                        d.get_attribute(element, "src").map(str::to_string),
                        d.get_attribute(element, "srcdoc").map(str::to_string),
                    )
                };
                let changed = {
                    let attrs = info.iframe_attrs.borrow();
                    match attrs.iter().find(|(n, _, _)| *n == element) {
                        Some((_, s, sd)) => *s != src || *sd != srcdoc,
                        None => true,
                    }
                };
                if changed {
                    info.iframe_attrs
                        .borrow_mut()
                        .retain(|(n, _, _)| *n != element);
                    let srcdoc_removed_only = srcdoc.is_none() && src.is_none();
                    info.iframe_attrs.borrow_mut().push((element, src, srcdoc));
                    if !srcdoc_removed_only {
                        process_iframe_attributes(agent, doc, element, ctx.id, false);
                    }
                }
            }
            (false, None) => {}
        }
    }
}

// ----- auxiliary browsing contexts -------------------------------------------

/// `window.open(url, target, features)` from the window of `state`:
/// the `WindowProxy` of the chosen context, or `null`.
pub fn window_open(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    url: &str,
    target: &str,
    features: &str,
) -> Result<Value, crate::dom::DomException> {
    let Some(ua) = super::ua() else {
        return Ok(Value::Null);
    };
    let Some(source) = state.html.context.get() else {
        return Ok(Value::Null);
    };
    let base = state
        .document()
        .map(|d| super::document::base_url(&d))
        .unwrap_or_else(|| Url::parse("about:blank", None).expect("about:blank parses"));
    let parsed = if url.is_empty() {
        None
    } else {
        Some(Url::parse(url, Some(&base)).map_err(|_| crate::dom::DomException::SyntaxError)?)
    };
    let tokens: Vec<String> = features
        .split([',', ' ', '\t', '\n', '\x0c', '\r'])
        .filter(|t| !t.is_empty())
        .map(|t| t.split('=').next().unwrap_or("").to_ascii_lowercase())
        .collect();
    let noopener = tokens.iter().any(|t| t == "noopener" || t == "noreferrer");
    let target_lower = target.to_ascii_lowercase();
    let chosen = if target.is_empty() {
        None
    } else {
        context_for_target(&ua, source, target)
    };
    let (ctx, new) = match chosen {
        Some(c) => (c, false),
        None => {
            let name = if target_lower == "_blank" { "" } else { target };
            let id = create_top_level(agent, if noopener { None } else { Some(source) }, name);
            let c = ua.context(id).expect("the new context exists");
            c.script_closable.set(true);
            (c, true)
        }
    };
    if let Some(u) = parsed {
        let blank = u.href() == "about:blank";
        if !(new && blank) {
            let mut p = NavigateParams::new(u);
            p.source_origin = state.document().map(|d| d.borrow().origin.clone());
            navigate(agent, ctx.id, p);
        }
    }
    if noopener {
        return Ok(Value::Null);
    }
    Ok(Value::Object(ctx.proxy))
}

/// The browsing context a link, form or `window.open` with target name
/// `target` picks from `source` ("the rules for choosing a navigable",
/// without creating one): `""` and `_self` are `source`, `_parent` and
/// `_top` its ancestors, `_blank` none, any other name the context with
/// that name.
pub fn context_for_target(
    ua: &UserAgent,
    source: ContextId,
    target: &str,
) -> Option<Rc<BrowsingContext>> {
    match target.to_ascii_lowercase().as_str() {
        "" | "_self" => ua.context(source),
        "_blank" => None,
        "_parent" => ua
            .context(source)
            .and_then(|c| c.parent.and_then(|p| ua.context(p)))
            .or_else(|| ua.context(source)),
        "_top" => {
            let mut c = ua.context(source);
            while let Some(p) = c
                .as_ref()
                .and_then(|x| x.parent)
                .and_then(|p| ua.context(p))
            {
                c = Some(p);
            }
            c
        }
        _ => ua
            .contexts
            .borrow()
            .iter()
            .find(|c| !c.discarded.get() && *c.name.borrow() == target)
            .cloned(),
    }
}

/// Applies a session history traversal of `delta` steps to context `id`
/// (`history.go`). Entries of the same document update its URL and fire
/// `popstate` (and `hashchange`); others load their URL again.
pub fn traverse_history(agent: &mut Agent, id: ContextId, delta: i64) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(ctx) = ua.context(id) else {
        return;
    };
    let (target, len) = {
        let s = ctx.session.borrow();
        (s.index as i64 + delta, s.entries.len() as i64)
    };
    if delta == 0 {
        if let Some(doc) = ctx.active_document() {
            let url = doc.borrow().url.clone();
            let mut p = NavigateParams::new(url);
            p.replace = true;
            navigate(agent, id, p);
        }
        return;
    }
    if target < 0 || target >= len {
        return;
    }
    queue_task(TaskSource::HistoryTraversal, None, move |agent| {
        let Some(ua) = super::ua() else {
            return;
        };
        let Some(ctx) = ua.context(id) else {
            return;
        };
        let Some(w) = ctx.active_window() else {
            return;
        };
        let (url, state, same_doc) = {
            let mut s = ctx.session.borrow_mut();
            if target as usize >= s.entries.len() {
                return;
            }
            s.index = target as usize;
            let e = &s.entries[s.index];
            let same = e
                .document
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|d| Rc::ptr_eq(&d, &w.doc));
            (e.url.clone(), e.state.clone(), same)
        };
        if !same_doc {
            let mut p = NavigateParams::new(url);
            p.replace = true;
            // Keep the entry list: a traversal does not prune it.
            let nav = new_navigation_id();
            ctx.navigation.set(nav);
            super::loader::start_navigation(
                agent,
                &ua,
                super::loader::NavigationRequest {
                    context: id,
                    navigation: nav,
                    url: p.url,
                    replace: true,
                    srcdoc: None,
                    request: None,
                },
            );
            return;
        }
        let old = w.doc.borrow().url.clone();
        w.doc.borrow_mut().url = url.clone();
        let value = match &state {
            Some(s) => super::clone::deserialize(agent, w.realm, s).unwrap_or(Value::Null),
            None => Value::Null,
        };
        super::window::set_history_state(agent, &w.state, value);
        fire_window_event(
            agent,
            &w.state,
            "popstate",
            EventInterface::Other("PopStateEvent"),
            EventInit::default(),
            &[
                ("state", value),
                ("hasUAVisualTransition", Value::Boolean(false)),
            ],
            false,
        );
        if old.fragment != url.fragment {
            let o = Value::String(agent.heap.alloc_str(&old.href()));
            let n = Value::String(agent.heap.alloc_str(&url.href()));
            fire_window_event(
                agent,
                &w.state,
                "hashchange",
                EventInterface::Other("HashChangeEvent"),
                EventInit::default(),
                &[("oldURL", o), ("newURL", n)],
                false,
            );
        }
    });
}
