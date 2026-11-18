//! `script` elements (HTML §4.12.1): "prepare the script element" with its
//! type, `nomodule`, `async`/`defer` and the lists of scripts of a
//! document; fetching classic scripts; "execute the script element" with
//! `document.currentScript`, the ignore-destructive-writes counter and the
//! `load`/`error` events; and "run a classic script" (HTML §8.1.4.4).
//! Module scripts are fetched and linked by [`super::module`].

use std::rc::Rc;

use vjs::gc::{Object, Realm, Trace, Tracer};
use vjs::{Agent, Throw, Value};

use super::event_loop::{TaskSource, clean_up_after_running_script, queue_task, queue_value_task};
use super::navigable::fire_simple_event;
use super::{UserAgent, in_realm};
use crate::bindings::RealmState;
use crate::dom::events::{EventInit, Target};
use crate::dom::{DocRef, NodeId};
use crate::encoding::Encoding;
use crate::url::Url;

pub type ScriptId = u64;

/// The type of a script element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptType {
    Classic,
    Module,
    ImportMap,
}

/// The result of a script element.
pub enum ScriptResult {
    Uninitialized,
    /// Fetching failed: executing the element fires `error`.
    Null,
    Classic {
        source: Rc<str>,
        /// The base URL of the script (its response URL, or the document
        /// base URL of an inline script).
        base: String,
    },
    Module {
        /// `None` when the root itself failed to parse.
        record: Option<Object>,
        /// The parse or link error of its graph, reported when it runs.
        error_to_rethrow: Option<Value>,
    },
    ImportMap {
        text: String,
        base: Url,
    },
}

/// What happens when the result becomes ready (the list of the document
/// the element was put in).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnReady {
    Nothing,
    ParsingBlocking,
    Defer,
    InOrder,
    Asap,
}

/// A prepared script element.
pub struct ScriptEntry {
    pub doc: DocRef,
    pub element: NodeId,
    pub kind: ScriptType,
    pub result: ScriptResult,
    pub ready: bool,
    pub from_external_file: bool,
    pub realm: Option<Realm>,
    pub on_ready: OnReady,
}

impl Trace for ScriptEntry {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(r) = self.realm {
            tracer.realm(r);
        }
        if let ScriptResult::Module {
            record,
            error_to_rethrow,
        } = &self.result
        {
            if let Some(r) = record {
                tracer.object(*r);
            }
            if let Some(e) = error_to_rethrow {
                tracer.value(*e);
            }
        }
    }
}

/// True if the result of script `id` is ready (a script that is gone
/// counts as ready so that nothing waits for it forever).
pub fn is_ready(ua: &UserAgent, id: ScriptId) -> bool {
    ua.scripts.borrow().get(&id).is_none_or(|e| e.ready)
}

/// ASCII whitespace as HTML defines it.
fn is_ascii_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ')
}

/// The type of a script block from its `type` and `language` attributes
/// (step 8 of "prepare the script element"); `None` for a data block.
pub fn script_block_type(type_attr: Option<&str>, language: Option<&str>) -> Option<ScriptType> {
    let ty = match (type_attr, language) {
        (Some(""), _) | (None, Some("")) | (None, None) => "text/javascript".to_string(),
        (Some(t), _) => t.trim_matches(is_ascii_ws).to_string(),
        (None, Some(l)) => format!("text/{l}"),
    };
    if crate::mime::is_javascript_mime_type_essence(&ty) {
        Some(ScriptType::Classic)
    } else if ty.eq_ignore_ascii_case("module") {
        Some(ScriptType::Module)
    } else if ty.eq_ignore_ascii_case("importmap") {
        Some(ScriptType::ImportMap)
    } else {
        None
    }
}

/// `HTMLScriptElement.supports(type)`.
pub fn supports(ty: &str) -> bool {
    matches!(ty, "classic" | "module" | "importmap")
}

/// The attributes and state "prepare the script element" reads.
struct Prep {
    already_started: bool,
    parser_inserted: bool,
    connected: bool,
    has_src: bool,
    src: String,
    has_async: bool,
    has_defer: bool,
    nomodule: bool,
    type_attr: Option<String>,
    language: Option<String>,
    event: Option<String>,
    for_: Option<String>,
    charset: Option<String>,
    source_text: String,
}

fn read_prep(doc: &DocRef, el: NodeId) -> Option<Prep> {
    let d = doc.borrow();
    let e = d.element(el)?;
    let attr = |n: &str| e.attr(n).map(str::to_string);
    Some(Prep {
        already_started: e.script.already_started,
        parser_inserted: e.script.parser_inserted,
        connected: d.is_connected(el),
        has_src: e.attr("src").is_some(),
        src: e.attr("src").unwrap_or("").to_string(),
        has_async: e.attr("async").is_some(),
        has_defer: e.attr("defer").is_some(),
        nomodule: e.attr("nomodule").is_some(),
        type_attr: attr("type"),
        language: attr("language"),
        event: attr("event"),
        for_: attr("for"),
        charset: attr("charset"),
        source_text: d.child_text_content(el),
    })
}

fn set_flags(doc: &DocRef, el: NodeId, f: impl FnOnce(&mut crate::dom::ScriptFlags)) {
    if let Some(e) = doc.borrow_mut().element_mut(el) {
        f(&mut e.script);
    }
}

/// Queues an element task that fires `error` at `el`.
fn queue_error_event(doc: &DocRef, el: NodeId) {
    let d = doc.clone();
    queue_task(
        TaskSource::DomManipulation,
        Some(doc.clone()),
        move |agent| {
            fire_simple_event(agent, &d, Target::Node(el), "error", EventInit::default());
        },
    );
}

fn new_script_id(ua: &UserAgent) -> ScriptId {
    let id = ua.next_script.get();
    ua.next_script.set(id + 1);
    id
}

/// "Prepare the script element" `el` of `doc` (HTML §4.12.1.1).
pub fn prepare_script(agent: &mut Agent, doc: &DocRef, el: NodeId) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(p) = read_prep(doc, el) else {
        return;
    };
    if p.already_started {
        return;
    }
    let parser_document = p.parser_inserted;
    set_flags(doc, el, |f| f.parser_inserted = false);
    if parser_document && !p.has_async {
        set_flags(doc, el, |f| f.force_async = true);
    }
    if !p.has_src && p.source_text.is_empty() {
        if parser_document {
            set_flags(doc, el, |f| {
                f.parser_inserted = true;
                f.force_async = false;
            });
        }
        return;
    }
    if !p.connected {
        return;
    }
    let Some(kind) = script_block_type(p.type_attr.as_deref(), p.language.as_deref()) else {
        return;
    };
    if parser_document {
        set_flags(doc, el, |f| {
            f.parser_inserted = true;
            f.force_async = false;
        });
    }
    set_flags(doc, el, |f| f.already_started = true);
    if !ua.scripting_enabled(doc) {
        return;
    }
    if p.nomodule && kind == ScriptType::Classic {
        return;
    }
    if kind == ScriptType::Classic {
        if let (Some(event), Some(for_)) = (&p.event, &p.for_) {
            let for_ = for_.trim_matches(is_ascii_ws);
            let event = event.trim_matches(is_ascii_ws);
            if !for_.eq_ignore_ascii_case("window") {
                return;
            }
            if !(event.eq_ignore_ascii_case("onload") || event.eq_ignore_ascii_case("onload()")) {
                return;
            }
        }
    }
    let Some(info) = ua.doc_info(doc) else {
        return;
    };
    let fallback_encoding = doc.borrow().encoding;
    let encoding = p
        .charset
        .as_deref()
        .and_then(Encoding::for_label)
        .unwrap_or(fallback_encoding);
    let realm = info.realm.get();
    let id = new_script_id(&ua);
    ua.scripts.borrow_mut().insert(
        id,
        ScriptEntry {
            doc: doc.clone(),
            element: el,
            kind,
            result: ScriptResult::Uninitialized,
            ready: false,
            from_external_file: p.has_src,
            realm,
            on_ready: OnReady::Nothing,
        },
    );
    let base = super::document::base_url(doc);
    if p.has_src {
        if kind == ScriptType::ImportMap {
            ua.scripts.borrow_mut().remove(&id);
            queue_error_event(doc, el);
            return;
        }
        if p.src.is_empty() {
            ua.scripts.borrow_mut().remove(&id);
            queue_error_event(doc, el);
            return;
        }
        let Ok(url) = Url::parse(&p.src, Some(&base)) else {
            ua.scripts.borrow_mut().remove(&id);
            queue_error_event(doc, el);
            return;
        };
        match kind {
            ScriptType::Classic => fetch_classic_script(&ua, id, doc, url, encoding),
            ScriptType::Module => {
                if let Some(realm) = realm {
                    super::module::fetch_external_module_graph(agent, &ua, realm, doc, url, id);
                }
            }
            ScriptType::ImportMap => {}
        }
    } else {
        match kind {
            ScriptType::Classic => {
                if let Some(e) = ua.scripts.borrow_mut().get_mut(&id) {
                    e.result = ScriptResult::Classic {
                        source: Rc::from(p.source_text.as_str()),
                        base: base.href(),
                    };
                    e.ready = true;
                }
            }
            ScriptType::Module => {
                if let Some(realm) = realm {
                    super::module::fetch_inline_module_graph(
                        agent,
                        &ua,
                        realm,
                        doc,
                        &p.source_text,
                        base,
                        id,
                    );
                }
            }
            ScriptType::ImportMap => {
                if let Some(e) = ua.scripts.borrow_mut().get_mut(&id) {
                    e.result = ScriptResult::ImportMap {
                        text: p.source_text.clone(),
                        base,
                    };
                    e.ready = true;
                }
            }
        }
    }
    // Step 32: where the element waits.
    let force_async = doc
        .borrow()
        .element(el)
        .is_some_and(|e| e.script.force_async);
    if (kind == ScriptType::Classic && p.has_src) || kind == ScriptType::Module {
        let on_ready = if p.has_async || force_async {
            info.asap_scripts.borrow_mut().push(id);
            OnReady::Asap
        } else if !parser_document {
            info.in_order_scripts.borrow_mut().push(id);
            OnReady::InOrder
        } else if kind == ScriptType::Module || p.has_defer {
            info.defer_scripts.borrow_mut().push(id);
            OnReady::Defer
        } else {
            info.pending_parsing_blocking.set(Some(id));
            OnReady::ParsingBlocking
        };
        let ready = match ua.scripts.borrow_mut().get_mut(&id) {
            Some(e) => {
                e.on_ready = on_ready;
                e.ready
            }
            None => false,
        };
        if ready && on_ready != OnReady::ParsingBlocking && on_ready != OnReady::Defer {
            // Ready already (a fetch that failed synchronously).
            script_ready(agent, id);
        }
    } else {
        // An inline classic script or an import map: no style sheet ever
        // blocks scripts, so it runs now.
        execute_script_element(agent, id);
    }
}

/// "Fetch a classic script" for script `id`.
fn fetch_classic_script(ua: &UserAgent, id: ScriptId, doc: &DocRef, url: Url, encoding: Encoding) {
    super::loader::fetch(ua, url, Some(doc.clone()), move |agent, result| {
        let result = match result {
            Ok(r) if (200..300).contains(&r.status) => {
                let source =
                    super::loader::decode_script(&r.body, r.content_type.as_deref(), encoding);
                ScriptResult::Classic {
                    source: Rc::from(source.as_str()),
                    base: r.url.href(),
                }
            }
            _ => ScriptResult::Null,
        };
        mark_ready(agent, id, result);
    });
}

/// "Mark as ready": sets the result of script `id` and runs what was
/// waiting for it.
pub fn mark_ready(agent: &mut Agent, id: ScriptId, result: ScriptResult) {
    let Some(ua) = super::ua() else {
        return;
    };
    {
        let mut scripts = ua.scripts.borrow_mut();
        let Some(e) = scripts.get_mut(&id) else {
            return;
        };
        e.result = result;
        e.ready = true;
    }
    script_ready(agent, id);
}

/// Runs the steps the element's list asks for once script `id` is ready.
fn script_ready(agent: &mut Agent, id: ScriptId) {
    let Some(ua) = super::ua() else {
        return;
    };
    let (doc, on_ready) = match ua.scripts.borrow().get(&id) {
        Some(e) => (e.doc.clone(), e.on_ready),
        None => return,
    };
    let Some(info) = ua.doc_info(&doc) else {
        return;
    };
    match on_ready {
        OnReady::Nothing => {}
        OnReady::Asap => {
            execute_script_element(agent, id);
            info.asap_scripts.borrow_mut().retain(|s| *s != id);
            super::document::process_html_changes(agent, &doc);
            super::document::continue_the_end(agent, &doc);
        }
        OnReady::InOrder => {
            loop {
                let first = info.in_order_scripts.borrow().first().copied();
                match first {
                    Some(f) if is_ready(&ua, f) => {
                        info.in_order_scripts.borrow_mut().remove(0);
                        execute_script_element(agent, f);
                        super::document::process_html_changes(agent, &doc);
                    }
                    _ => break,
                }
            }
            super::document::continue_the_end(agent, &doc);
        }
        OnReady::Defer => super::document::continue_the_end(agent, &doc),
        OnReady::ParsingBlocking => {
            if info.pending_parsing_blocking.get() == Some(id) {
                super::document::resume_parser_soon(&doc);
            }
        }
    }
}

/// Marks script `id` ready from a microtask (module graphs): the steps
/// run in a networking task, as fetch completions do. `values` keeps the
/// result alive until then.
pub fn mark_ready_soon(id: ScriptId, doc: &DocRef, record: Option<Object>, error: Option<Value>) {
    let mut values = Vec::new();
    if let Some(r) = record {
        values.push(Value::Object(r));
    }
    if let Some(e) = error {
        values.push(e);
    }
    queue_value_task(
        TaskSource::Networking,
        Some(doc.clone()),
        values,
        move |agent, _values| {
            let result = match (record, error) {
                (None, None) => ScriptResult::Null,
                (record, error) => ScriptResult::Module {
                    record,
                    error_to_rethrow: error,
                },
            };
            mark_ready(agent, id, result);
        },
    );
}

/// "Execute the script element" of script `id` (HTML §4.12.1.1); the
/// entry is dropped afterwards.
pub fn execute_script_element(agent: &mut Agent, id: ScriptId) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(entry) = ua.scripts.borrow_mut().remove(&id) else {
        return;
    };
    let doc = entry.doc.clone();
    let el = entry.element;
    let Some(info) = ua.doc_info(&doc) else {
        return;
    };
    if matches!(
        entry.result,
        ScriptResult::Null | ScriptResult::Uninitialized
    ) {
        fire_simple_event(agent, &doc, Target::Node(el), "error", EventInit::default());
        return;
    }
    let Some(state) = entry
        .realm
        .and_then(|r| crate::bindings::state_of(agent, r))
    else {
        return;
    };
    // Keep the module record rooted while it runs (the entry left the map).
    let keep = Rc::new(EntryRoots(entry));
    let root = agent.heap.add_root_set(keep.clone() as Rc<dyn Trace>);
    let entry = &keep.0;
    let counts = entry.from_external_file || entry.kind == ScriptType::Module;
    if counts {
        info.ignore_destructive_writes
            .set(info.ignore_destructive_writes.get() + 1);
    }
    match &entry.result {
        ScriptResult::Classic { source, base } => {
            let old = info.current_script.get();
            let in_shadow = {
                let d = doc.borrow();
                let root = d.root(el);
                d.is_shadow_root(root)
            };
            info.current_script
                .set(if in_shadow { None } else { Some(el) });
            run_classic_source(agent, &state, source, Some(base.as_str()));
            info.current_script.set(old);
        }
        ScriptResult::Module {
            record,
            error_to_rethrow,
        } => {
            let old = info.current_script.get();
            info.current_script.set(None);
            super::module::run_module_script(agent, &state, *record, *error_to_rethrow);
            info.current_script.set(old);
        }
        ScriptResult::ImportMap { text, base } => {
            super::module::register_import_map(agent, &state, text, base);
        }
        ScriptResult::Null | ScriptResult::Uninitialized => {}
    }
    if counts {
        info.ignore_destructive_writes
            .set(info.ignore_destructive_writes.get().saturating_sub(1));
    }
    agent.heap.remove_root_set(root);
    if entry.from_external_file {
        fire_simple_event(agent, &doc, Target::Node(el), "load", EventInit::default());
        clean_up_after_running_script(agent);
    }
}

struct EntryRoots(ScriptEntry);

impl Trace for EntryRoots {
    fn trace(&self, tracer: &mut Tracer) {
        self.0.trace(tracer);
    }
}

/// "Run a classic script" with `source` in the window of `state`: the
/// script's base URL (`base`, or the document base URL) is its record's
/// `[[HostDefined]]`, the referrer of its `import()` calls. An exception
/// is reported; a microtask checkpoint follows when the stack is empty.
pub fn run_classic_source(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    source: &str,
    base: Option<&str>,
) {
    let Some(ua) = super::ua() else {
        return;
    };
    if !ua.options.scripting {
        return;
    }
    let realm = state.realm;
    let base = match base {
        Some(b) => b.to_string(),
        None => state
            .document()
            .map(|d| super::document::base_url(&d).href())
            .unwrap_or_else(|| "about:blank".to_string()),
    };
    let record = vjs::module::create_script_record(agent, realm, Some(Rc::from(base.as_str())));
    let result = vjs::evaluate_script_record(agent, realm, source, Some(record));
    if let Err(Throw(e)) = result {
        super::errors::report_exception(agent, state, e);
    }
    clean_up_after_running_script(agent);
}

/// Evaluates `source` as a classic script of `state`'s window and returns
/// its completion (the exception is not reported): for `javascript:`
/// URLs and the embedder.
pub fn evaluate_classic(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    source: &str,
) -> vjs::JsResult<Value> {
    let realm = state.realm;
    let base = state
        .document()
        .map(|d| super::document::base_url(&d).href())
        .unwrap_or_else(|| "about:blank".to_string());
    let record = vjs::module::create_script_record(agent, realm, Some(Rc::from(base.as_str())));
    in_realm(agent, realm, |agent| {
        vjs::evaluate_script_record(agent, realm, source, Some(record))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_types() {
        assert_eq!(script_block_type(None, None), Some(ScriptType::Classic));
        assert_eq!(script_block_type(Some(""), None), Some(ScriptType::Classic));
        assert_eq!(
            script_block_type(Some(" text/JavaScript "), None),
            Some(ScriptType::Classic)
        );
        assert_eq!(
            script_block_type(Some("MODULE"), None),
            Some(ScriptType::Module)
        );
        assert_eq!(
            script_block_type(Some("importmap"), None),
            Some(ScriptType::ImportMap)
        );
        assert_eq!(
            script_block_type(None, Some("javascript1.5")),
            Some(ScriptType::Classic)
        );
        assert_eq!(script_block_type(None, Some("")), Some(ScriptType::Classic));
        assert_eq!(script_block_type(Some("text/plain"), None), None);
        assert_eq!(
            script_block_type(Some("text/javascript; charset=utf-8"), None),
            None
        );
    }

    #[test]
    fn supported_script_types() {
        assert!(supports("classic"));
        assert!(supports("module"));
        assert!(supports("importmap"));
        assert!(!supports("speculationrules"));
        assert!(!supports("Module"));
    }
}
