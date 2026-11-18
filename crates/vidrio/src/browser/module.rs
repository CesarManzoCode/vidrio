//! Module scripts (HTML §8.1.4.2, §8.1.5.5): the module map of each
//! window, fetching module scripts and their graphs through the host hooks
//! of vjs ([`HtmlModuleHost`]: HostLoadImportedModule and `import.meta`),
//! import maps (HTML §8.1.5.4: parsing, merging and "resolve a module
//! specifier"), and running a module script.
//!
//! A record's `[[HostDefined]]` is its base URL: the response URL of a
//! fetched module, the document base URL of an inline one or of a classic
//! script (the referrer of its `import()` calls).

use std::collections::HashMap;
use std::rc::Rc;

use vjs::agent::ErrorKind;
use vjs::function::{NativeCall, native_slots};
use vjs::gc::{Realm, Trace, Tracer};
use vjs::module::{LoadPayload, ModuleHost, ModuleRequest, Referrer};
use vjs::{Agent, JsResult, Object, PropertyKey, Throw, Value};

use super::script::ScriptId;
use super::{UserAgent, in_realm};
use crate::bindings::RealmState;
use crate::dom::DocRef;
use crate::url::Url;

// ----- import maps -----------------------------------------------------------------------

/// A specifier map: keys in descending code unit order, with their
/// addresses (`None`: blocked).
pub type SpecifierMap = Vec<(String, Option<Url>)>;

/// An import map.
#[derive(Clone, Debug, Default)]
pub struct ImportMap {
    pub imports: SpecifierMap,
    /// Scope prefixes (serialized URLs) in descending code unit order.
    pub scopes: Vec<(String, SpecifierMap)>,
}

/// "Resolve a URL-like module specifier".
pub fn resolve_url_like(specifier: &str, base: &Url) -> Option<Url> {
    if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        return Url::parse(specifier, Some(base)).ok();
    }
    Url::parse(specifier, None).ok()
}

fn sort_map(map: &mut SpecifierMap) {
    map.sort_by(|a, b| b.0.encode_utf16().cmp(a.0.encode_utf16()));
}

/// "Resolve an imports match".
fn resolve_imports_match(
    normalized: &str,
    as_url: Option<&Url>,
    map: &SpecifierMap,
) -> Result<Option<Url>, String> {
    for (key, value) in map {
        if key == normalized {
            return match value {
                Some(u) => Ok(Some(u.clone())),
                None => Err(format!("the import map blocks \"{normalized}\"")),
            };
        }
        let special = as_url.is_none_or(Url::is_special);
        if key.ends_with('/') && normalized.starts_with(key.as_str()) && special {
            let Some(address) = value else {
                return Err(format!("the import map blocks \"{normalized}\""));
            };
            let after = &normalized[key.len()..];
            let Ok(url) = Url::parse(after, Some(address)) else {
                return Err(format!(
                    "\"{normalized}\" does not resolve against its import map entry"
                ));
            };
            if !url.href().starts_with(&address.href()) {
                return Err(format!("\"{normalized}\" backtracks above its prefix"));
            }
            return Ok(Some(url));
        }
    }
    Ok(None)
}

/// "Resolve a module specifier" with the import map `map`.
pub fn resolve_with_map(map: &ImportMap, specifier: &str, base: &Url) -> Result<Url, String> {
    let as_url = resolve_url_like(specifier, base);
    let normalized = as_url
        .as_ref()
        .map_or_else(|| specifier.to_string(), Url::href);
    let base_href = base.href();
    for (prefix, imports) in &map.scopes {
        if *prefix == base_href || (prefix.ends_with('/') && base_href.starts_with(prefix.as_str()))
        {
            if let Some(u) = resolve_imports_match(&normalized, as_url.as_ref(), imports)? {
                return Ok(u);
            }
        }
    }
    if let Some(u) = resolve_imports_match(&normalized, as_url.as_ref(), &map.imports)? {
        return Ok(u);
    }
    as_url.ok_or_else(|| format!("Failed to resolve module specifier \"{specifier}\""))
}

/// "Resolve a module specifier" in the window of `state`.
pub fn resolve_module_specifier(
    state: &RealmState,
    specifier: &str,
    base: &Url,
) -> Result<Url, String> {
    let map = state.html.import_map.borrow();
    resolve_with_map(&map, specifier, base)
}

/// The own enumerable string-keyed properties of `o`.
fn entries(agent: &mut Agent, o: Object) -> JsResult<Vec<(String, Value)>> {
    let keys = vjs::internal::own_property_keys(agent, o)?;
    let mut out = Vec::new();
    for key in keys {
        if matches!(key, PropertyKey::Symbol(_)) {
            continue;
        }
        let Some(desc) = vjs::internal::get_own_property(agent, o, key)? else {
            continue;
        };
        if desc.enumerable != Some(true) {
            continue;
        }
        let v = vjs::ops::get(agent, o, key)?;
        let name = agent.heap.key_to_rust(key);
        out.push((name, v));
    }
    Ok(out)
}

/// "Sort and normalize a specifier map".
fn normalize_map(agent: &mut Agent, o: Object, base: &Url) -> JsResult<SpecifierMap> {
    let mut map = Vec::new();
    for (key, value) in entries(agent, o)? {
        if key.is_empty() {
            continue;
        }
        let normalized = resolve_url_like(&key, base).map_or(key.clone(), |u| u.href());
        let address = match value {
            Value::String(s) => {
                let s = agent.heap.string_to_rust(s);
                resolve_url_like(&s, base)
            }
            _ => None,
        };
        let address = match address {
            Some(a) if key.ends_with('/') && !a.href().ends_with('/') => None,
            a => a,
        };
        map.push((normalized, address));
    }
    sort_map(&mut map);
    Ok(map)
}

/// "Parse an import map string" `text` with base URL `base`.
pub fn parse_import_map(
    agent: &mut Agent,
    realm: Realm,
    text: &str,
    base: &Url,
) -> JsResult<ImportMap> {
    in_realm(agent, realm, |agent| {
        let units: Vec<u16> = text.encode_utf16().collect();
        let parsed = vjs::json::parse_json(agent, &units)?;
        let Value::Object(o) = parsed else {
            return agent.type_error("The import map is not a JSON object.");
        };
        if vjs::ops::is_array(agent, parsed)? {
            return agent.type_error("The import map is not a JSON object.");
        }
        let mut map = ImportMap::default();
        let key = agent.heap.key_from_str("imports");
        let imports = vjs::ops::get(agent, o, key)?;
        match imports {
            Value::Undefined => {}
            Value::Object(i) if !vjs::ops::is_array(agent, imports)? => {
                map.imports = normalize_map(agent, i, base)?;
            }
            _ => return agent.type_error("The import map's \"imports\" is not an object."),
        }
        let key = agent.heap.key_from_str("scopes");
        let scopes = vjs::ops::get(agent, o, key)?;
        match scopes {
            Value::Undefined => {}
            Value::Object(s) if !vjs::ops::is_array(agent, scopes)? => {
                for (prefix, value) in entries(agent, s)? {
                    let Value::Object(m) = value else {
                        return agent.type_error("A scope of the import map is not an object.");
                    };
                    let Ok(prefix_url) = Url::parse(&prefix, Some(base)) else {
                        continue;
                    };
                    let m = normalize_map(agent, m, base)?;
                    map.scopes.push((prefix_url.href(), m));
                }
                map.scopes
                    .sort_by(|a, b| b.0.encode_utf16().cmp(a.0.encode_utf16()));
            }
            _ => return agent.type_error("The import map's \"scopes\" is not an object."),
        }
        Ok(map)
    })
}

/// "Merge existing and new import maps": entries already defined win.
pub fn merge_import_maps(existing: &mut ImportMap, new: ImportMap) {
    for (prefix, map) in new.scopes {
        match existing.scopes.iter_mut().find(|(p, _)| *p == prefix) {
            Some((_, old)) => {
                for (k, v) in map {
                    if !old.iter().any(|(ok, _)| *ok == k) {
                        old.push((k, v));
                    }
                }
                sort_map(old);
            }
            None => existing.scopes.push((prefix, map)),
        }
    }
    existing
        .scopes
        .sort_by(|a, b| b.0.encode_utf16().cmp(a.0.encode_utf16()));
    for (k, v) in new.imports {
        if !existing.imports.iter().any(|(ok, _)| *ok == k) {
            existing.imports.push((k, v));
        }
    }
    sort_map(&mut existing.imports);
}

/// "Register an import map" in the window of `state`.
pub fn register_import_map(agent: &mut Agent, state: &Rc<RealmState>, text: &str, base: &Url) {
    match parse_import_map(agent, state.realm, text, base) {
        Ok(map) => merge_import_maps(&mut state.html.import_map.borrow_mut(), map),
        Err(Throw(e)) => super::errors::report_exception(agent, state, e),
    }
}

// ----- the module map ------------------------------------------------------------------------

/// Who waits for a module script.
enum Waiter {
    /// HostLoadImportedModule.
    Load {
        referrer: Referrer,
        request: ModuleRequest,
        payload: LoadPayload,
    },
    /// The root of a `script type=module src` element.
    Root { script: ScriptId },
}

/// An entry of a module map.
enum Entry {
    Fetching(Vec<Waiter>),
    /// The module record, or `None` when fetching failed.
    Ready(Option<Object>),
    /// The parse error of the module.
    Error(Value),
}

/// The module maps of every window, keyed by realm, URL and type.
#[derive(Default)]
pub struct ModuleMaps {
    entries: HashMap<(Realm, String, String), Entry>,
    /// The `TypeError`s that stand for failed fetches (a script whose
    /// graph fails with one fires `error` instead of reporting it).
    fetch_failures: Vec<Value>,
}

impl Trace for ModuleMaps {
    fn trace(&self, tracer: &mut Tracer) {
        for ((realm, _, _), entry) in &self.entries {
            tracer.realm(*realm);
            match entry {
                Entry::Ready(Some(m)) => {
                    tracer.object(*m);
                }
                Entry::Error(v) => {
                    tracer.value(*v);
                }
                Entry::Fetching(waiters) => {
                    for w in waiters {
                        if let Waiter::Load {
                            referrer: Referrer::Record(o),
                            ..
                        } = w
                        {
                            tracer.object(*o);
                        }
                    }
                }
                Entry::Ready(None) => {}
            }
        }
        for v in &self.fetch_failures {
            tracer.value(*v);
        }
    }
}

/// The base URL a referrer's imports resolve against.
fn referrer_base(agent: &Agent, referrer: Referrer, realm: Realm) -> Url {
    if let Referrer::Record(m) = referrer {
        if let Some(h) = vjs::module::host_defined(agent, m) {
            if let Ok(u) = Url::parse(&h, None) {
                return u;
            }
        }
    }
    crate::bindings::state_of(agent, realm)
        .and_then(|s| s.document())
        .map(|d| super::document::base_url(&d))
        .unwrap_or_else(|| Url::parse("about:blank", None).expect("about:blank parses"))
}

fn fetch_failure(agent: &mut Agent, ua: &UserAgent, realm: Realm, url: &str) -> Throw {
    let e = in_realm(agent, realm, |agent| {
        agent.throw_error(
            ErrorKind::Type,
            &format!("Failed to fetch the module {url}"),
        )
    });
    ua.modules.borrow_mut().fetch_failures.push(e.0);
    e
}

/// Answers a waiter whose module map entry is settled.
fn answer(
    agent: &mut Agent,
    ua: &UserAgent,
    realm: Realm,
    url: &str,
    waiter: Waiter,
    outcome: &Result<Option<Object>, Value>,
) {
    match waiter {
        Waiter::Load {
            referrer,
            request,
            payload,
        } => {
            let result = match outcome {
                Ok(Some(m)) => Ok(*m),
                Ok(None) => Err(fetch_failure(agent, ua, realm, url)),
                Err(e) => Err(Throw(*e)),
            };
            vjs::module::finish_loading_imported_module(agent, referrer, &request, payload, result);
        }
        Waiter::Root { script } => {
            let doc = ua.scripts.borrow().get(&script).map(|e| e.doc.clone());
            let Some(doc) = doc else {
                return;
            };
            match outcome {
                Ok(Some(m)) => load_graph(agent, realm, *m, script),
                Ok(None) => super::script::mark_ready_soon(script, &doc, None, None),
                Err(e) => super::script::mark_ready_soon(script, &doc, None, Some(*e)),
            }
        }
    }
}

/// "Fetch a single module script" `url` of `module_type` for `realm`;
/// `waiter` is answered once it is in the module map.
fn fetch_single(
    agent: &mut Agent,
    ua: &UserAgent,
    realm: Realm,
    url: Url,
    module_type: &str,
    waiter: Waiter,
) {
    let key = (realm, url.href(), module_type.to_string());
    let mut waiter = Some(waiter);
    let settled = {
        let mut maps = ua.modules.borrow_mut();
        match maps.entries.get_mut(&key) {
            Some(Entry::Fetching(waiters)) => {
                waiters.extend(waiter.take());
                return;
            }
            Some(Entry::Ready(m)) => Some(Ok(*m)),
            Some(Entry::Error(e)) => Some(Err(*e)),
            None => {
                let list: Vec<Waiter> = waiter.take().into_iter().collect();
                maps.entries.insert(key.clone(), Entry::Fetching(list));
                None
            }
        }
    };
    if let Some(outcome) = settled {
        if let Some(w) = waiter.take() {
            answer(agent, ua, realm, &key.1, w, &outcome);
        }
        return;
    }
    let doc = super::window::document_of_realm(ua, realm);
    let module_type = module_type.to_string();
    super::loader::fetch(ua, url, doc, move |agent, result| {
        let Some(ua) = super::ua() else {
            return;
        };
        let href = key.1.clone();
        let outcome: Result<Option<Object>, Value> = match result {
            Ok(r) if (200..300).contains(&r.status) => {
                let essence = r
                    .content_type
                    .as_deref()
                    .and_then(crate::mime::MimeType::parse)
                    .map(|m| m.essence())
                    .unwrap_or_default();
                let source = crate::encoding::utf8_decode(&r.body);
                let host_defined: Option<Rc<str>> = Some(Rc::from(r.url.href().as_str()));
                if module_type == "json" {
                    if crate::mime::MimeType::parse(&essence).is_some_and(|m| m.is_json()) {
                        match vjs::module::create_json_module(agent, realm, &source, host_defined) {
                            Ok(m) => Ok(Some(m)),
                            Err(Throw(e)) => Err(e),
                        }
                    } else {
                        Ok(None)
                    }
                } else if crate::mime::is_javascript_mime_type_essence(&essence) {
                    match vjs::module::parse_module_record(agent, realm, &source, host_defined) {
                        Ok(m) => Ok(Some(m)),
                        Err(e) => Err(in_realm(agent, realm, |agent| {
                            Value::Object(agent.create_error(ErrorKind::Syntax, &e.message))
                        })),
                    }
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        };
        let waiters = {
            let mut maps = ua.modules.borrow_mut();
            let entry = match outcome {
                Ok(m) => Entry::Ready(m),
                Err(e) => Entry::Error(e),
            };
            match maps.entries.insert(key.clone(), entry) {
                Some(Entry::Fetching(w)) => w,
                _ => Vec::new(),
            }
        };
        for w in waiters {
            answer(agent, &ua, realm, &href, w, &outcome);
        }
    });
}

/// HostLoadImportedModule over fetch.
pub struct HtmlModuleHost;

impl ModuleHost for HtmlModuleHost {
    fn load_imported_module(
        &self,
        agent: &mut Agent,
        referrer: Referrer,
        request: &ModuleRequest,
        payload: LoadPayload,
    ) {
        let realm = match referrer {
            Referrer::Record(m) => vjs::module::record_realm(agent, m),
            Referrer::Realm(r) => r,
        };
        let fail = |agent: &mut Agent, message: &str| {
            let e = in_realm(agent, realm, |agent| {
                agent.throw_error(ErrorKind::Type, message)
            });
            vjs::module::finish_loading_imported_module(agent, referrer, request, payload, Err(e));
        };
        let (Some(ua), Some(state)) = (super::ua(), crate::bindings::state_of(agent, realm)) else {
            fail(agent, "modules need a window");
            return;
        };
        let base = referrer_base(agent, referrer, realm);
        let url = match resolve_module_specifier(&state, &request.specifier, &base) {
            Ok(u) => u,
            Err(message) => {
                fail(agent, &message);
                return;
            }
        };
        let module_type = request.module_type().unwrap_or("javascript").to_string();
        if module_type != "javascript" && module_type != "json" {
            fail(agent, &format!("unsupported module type \"{module_type}\""));
            return;
        }
        fetch_single(
            agent,
            &ua,
            realm,
            url,
            &module_type,
            Waiter::Load {
                referrer,
                request: request.clone(),
                payload,
            },
        );
    }

    fn import_meta_properties(
        &self,
        agent: &mut Agent,
        module: Object,
    ) -> Vec<(PropertyKey, Value)> {
        let url = vjs::module::host_defined(agent, module)
            .map(|h| h.to_string())
            .unwrap_or_default();
        let realm = vjs::module::record_realm(agent, module);
        let url_value = Value::String(agent.heap.alloc_str(&url));
        let name = agent.heap.key_from_str("resolve");
        let resolve = crate::bindings::install::make_fn(
            agent,
            realm,
            import_meta_resolve,
            1,
            name,
            None,
            vec![url_value],
        );
        let url_key = agent.heap.key_from_str("url");
        let resolve_key = agent.heap.key_from_str("resolve");
        vec![(url_key, url_value), (resolve_key, Value::Object(resolve))]
    }
}

/// `import.meta.resolve(specifier)`; slot 0 is the module's URL.
fn import_meta_resolve(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let base = native_slots(agent, call.callee)
        .first()
        .copied()
        .unwrap_or(Value::Undefined);
    let base = match base {
        Value::String(s) => agent.heap.string_to_rust(s),
        _ => String::new(),
    };
    let specifier = vjs::ops::to_string(agent, call.arg(0))?;
    let specifier = agent.heap.string_to_rust(specifier);
    let Ok(base) = Url::parse(&base, None) else {
        return agent.type_error("import.meta.resolve: the module has no URL");
    };
    let state = crate::bindings::current_state(agent)?;
    match resolve_module_specifier(&state, &specifier, &base) {
        Ok(u) => Ok(Value::String(agent.heap.alloc_str(&u.href()))),
        Err(message) => agent.type_error(&message),
    }
}

// ----- module script graphs ----------------------------------------------------------------------

fn slot_script(agent: &Agent, f: Object) -> (ScriptId, Option<Object>) {
    let slots = native_slots(agent, f);
    let id = slots.first().and_then(|v| v.as_number()).unwrap_or(0.0) as ScriptId;
    let m = slots.get(1).and_then(|v| v.as_object());
    (id, m)
}

fn script_doc(id: ScriptId) -> Option<DocRef> {
    super::ua()?
        .scripts
        .borrow()
        .get(&id)
        .map(|e| e.doc.clone())
}

/// The graph of `record` loaded: link it and mark the script ready.
fn graph_loaded(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (id, m) = slot_script(agent, call.callee);
    let (Some(m), Some(doc)) = (m, script_doc(id)) else {
        return Ok(Value::Undefined);
    };
    match vjs::module::link(agent, m) {
        Ok(()) => super::script::mark_ready_soon(id, &doc, Some(m), None),
        Err(Throw(e)) => super::script::mark_ready_soon(id, &doc, Some(m), Some(e)),
    }
    Ok(Value::Undefined)
}

/// Loading the graph failed: a fetch failure gives a null result, any
/// other error is rethrown when the script runs.
fn graph_failed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (id, m) = slot_script(agent, call.callee);
    let Some(doc) = script_doc(id) else {
        return Ok(Value::Undefined);
    };
    let reason = call.arg(0);
    let fetch_failed =
        super::ua().is_some_and(|ua| ua.modules.borrow().fetch_failures.contains(&reason));
    if fetch_failed {
        super::script::mark_ready_soon(id, &doc, None, None);
    } else {
        super::script::mark_ready_soon(id, &doc, m, Some(reason));
    }
    Ok(Value::Undefined)
}

/// "Fetch the descendants of and link" the module `record` of script
/// `id`.
fn load_graph(agent: &mut Agent, realm: Realm, record: Object, id: ScriptId) {
    let p = in_realm(agent, realm, |agent| {
        vjs::module::load_requested_modules(agent, record)
    });
    let Value::Object(p) = p else {
        return;
    };
    let slots = vec![Value::Number(id as f64), Value::Object(record)];
    let name = agent.heap.key_from_str("");
    let ok =
        crate::bindings::install::make_fn(agent, realm, graph_loaded, 0, name, None, slots.clone());
    let failed =
        crate::bindings::install::make_fn(agent, realm, graph_failed, 1, name, None, slots);
    vjs::interp::promise::perform_promise_then(
        agent,
        p,
        Value::Object(ok),
        Value::Object(failed),
        None,
    );
}

/// "Fetch an external module script graph" `url` for script `id`.
pub fn fetch_external_module_graph(
    agent: &mut Agent,
    ua: &UserAgent,
    realm: Realm,
    _doc: &DocRef,
    url: Url,
    id: ScriptId,
) {
    fetch_single(
        agent,
        ua,
        realm,
        url,
        "javascript",
        Waiter::Root { script: id },
    );
}

/// "Fetch an inline module script graph" for script `id`.
pub fn fetch_inline_module_graph(
    agent: &mut Agent,
    _ua: &UserAgent,
    realm: Realm,
    doc: &DocRef,
    source: &str,
    base: Url,
    id: ScriptId,
) {
    let host_defined: Option<Rc<str>> = Some(Rc::from(base.href().as_str()));
    match vjs::module::parse_module_record(agent, realm, source, host_defined) {
        Ok(m) => load_graph(agent, realm, m, id),
        Err(e) => {
            let error = in_realm(agent, realm, |agent| {
                Value::Object(agent.create_error(ErrorKind::Syntax, &e.message))
            });
            super::script::mark_ready_soon(id, doc, None, Some(error));
        }
    }
}

/// Reports the rejection of a module evaluation promise.
fn evaluation_rejected(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    super::errors::report_exception_in_current(agent, call.arg(0));
    Ok(Value::Undefined)
}

/// "Run a module script" in the window of `state`.
pub fn run_module_script(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    record: Option<Object>,
    error: Option<Value>,
) {
    if let Some(e) = error {
        super::errors::report_exception(agent, state, e);
        return;
    }
    let Some(m) = record else {
        return;
    };
    let realm = state.realm;
    let p = in_realm(agent, realm, |agent| vjs::module::evaluate(agent, m));
    if let Value::Object(p) = p {
        let name = agent.heap.key_from_str("");
        let on_rejected = crate::bindings::install::make_fn(
            agent,
            realm,
            evaluation_rejected,
            1,
            name,
            None,
            Vec::new(),
        );
        vjs::interp::promise::perform_promise_then(
            agent,
            p,
            Value::Undefined,
            Value::Object(on_rejected),
            None,
        );
    }
    super::event_loop::clean_up_after_running_script(agent);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s, None).unwrap()
    }

    #[test]
    fn bare_specifiers_need_a_map() {
        let base = url("http://a.test/dir/page.html");
        let map = ImportMap::default();
        assert_eq!(
            resolve_with_map(&map, "./x.js", &base).unwrap().href(),
            "http://a.test/dir/x.js"
        );
        assert_eq!(
            resolve_with_map(&map, "/y.js", &base).unwrap().href(),
            "http://a.test/y.js"
        );
        assert!(resolve_with_map(&map, "lodash", &base).is_err());
    }

    #[test]
    fn imports_and_scopes() {
        let base = url("http://a.test/dir/page.html");
        let mut map = ImportMap {
            imports: vec![
                (
                    "moment".to_string(),
                    Some(url("http://a.test/lib/moment.js")),
                ),
                ("lib/".to_string(), Some(url("http://a.test/lib/"))),
                ("blocked".to_string(), None),
            ],
            scopes: vec![(
                "http://a.test/dir/".to_string(),
                vec![(
                    "moment".to_string(),
                    Some(url("http://a.test/dir/moment2.js")),
                )],
            )],
        };
        sort_map(&mut map.imports);
        assert_eq!(
            resolve_with_map(&map, "moment", &base).unwrap().href(),
            "http://a.test/dir/moment2.js"
        );
        let other = url("http://a.test/other.html");
        assert_eq!(
            resolve_with_map(&map, "moment", &other).unwrap().href(),
            "http://a.test/lib/moment.js"
        );
        assert_eq!(
            resolve_with_map(&map, "lib/a/b.js", &other).unwrap().href(),
            "http://a.test/lib/a/b.js"
        );
        assert!(resolve_with_map(&map, "blocked", &other).is_err());
        assert!(resolve_with_map(&map, "lib/../../x.js", &other).is_err());
    }

    #[test]
    fn merging_keeps_existing_entries() {
        let mut a = ImportMap {
            imports: vec![("x".to_string(), Some(url("http://a.test/1.js")))],
            scopes: Vec::new(),
        };
        let b = ImportMap {
            imports: vec![
                ("x".to_string(), Some(url("http://a.test/2.js"))),
                ("y".to_string(), Some(url("http://a.test/3.js"))),
            ],
            scopes: Vec::new(),
        };
        merge_import_maps(&mut a, b);
        assert_eq!(a.imports.len(), 2);
        let x = a.imports.iter().find(|(k, _)| k == "x").unwrap();
        assert_eq!(x.1.as_ref().unwrap().href(), "http://a.test/1.js");
    }
}
