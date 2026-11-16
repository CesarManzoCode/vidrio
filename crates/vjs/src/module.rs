//! Scripts and modules (ECMA-262 §16.1, §16.2): Script Records, Source
//! Text Module Records, JSON modules (a synthetic module with one `default`
//! export), the loading of module graphs, linking, evaluation with
//! top-level `await`, `import()`, `import.meta` and module namespaces.
//!
//! Records are heap objects of kind [`ObjectKind::Module`] so that
//! execution contexts and functions can point to them
//! (`[[ScriptOrModule]]`). Every record stays alive for as long as the
//! agent: [`ModuleState`] roots them, as the host's module map would.
//!
//! The host provides the [`ModuleHost`] hooks. `HostLoadImportedModule`
//! receives a [`LoadPayload`] and answers, now or later, with
//! [`finish_loading_imported_module`]; the engine keeps the state behind
//! each payload rooted until then.
//!
//! The module environment is allocated with the record, so namespaces and
//! import bindings can point to bindings of modules of the same cycle
//! that are not initialized yet.

use std::collections::HashMap;
use std::rc::Rc;

use crate::agent::Agent;
use crate::ast::{ExportDecl, ImportSpecifier, Program, StmtKind, SyntaxError};
use crate::compiler::module::{DEFAULT_LOCAL, ModuleCode, compile_module};
use crate::completion::{JsResult, Throw};
use crate::env::{EnvData, ScopeNames};
use crate::exotic::{NamespaceBinding, module_namespace_create};
use crate::function::{NativeCall, create_native_function, native_slots};
use crate::gc::{Env, Object, Realm, Trace, Tracer};
use crate::interp::promise::{self, Capability};
use crate::interp::{Frame, FrameKind, global_env, run};
use crate::object::ObjectKind;
use crate::ops;
use crate::parser::{ParseOptions, parse_module};
use crate::value::{PropertyKey, Value};

// ----- records -----

/// ModuleRequest Record: a specifier and its import attributes, sorted by
/// key (two requests are equal when both are equal).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModuleRequest {
    pub specifier: String,
    pub attributes: Vec<(String, String)>,
}

impl ModuleRequest {
    pub fn new(specifier: String, mut attributes: Vec<(String, String)>) -> ModuleRequest {
        attributes.sort();
        ModuleRequest {
            specifier,
            attributes,
        }
    }

    /// The value of the `type` attribute, if any.
    pub fn module_type(&self) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == "type")
            .map(|(_, v)| v.as_str())
    }
}

/// `[[ImportName]]` of an ImportEntry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportName {
    Name(String),
    /// NAMESPACE-OBJECT.
    Namespace,
}

/// `[[ImportName]]` of an ExportEntry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportImportName {
    Name(String),
    /// ALL (`export * as ns from`).
    All,
    /// ALL-BUT-DEFAULT (`export * from`).
    AllButDefault,
}

#[derive(Clone, Debug)]
pub struct ImportEntry {
    /// Index in `requested`.
    pub request: usize,
    pub import_name: ImportName,
    pub local_name: String,
}

#[derive(Clone, Debug)]
pub struct ExportEntry {
    pub export_name: Option<String>,
    pub request: Option<usize>,
    pub import_name: Option<ExportImportName>,
    pub local_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleStatus {
    New,
    Unlinked,
    Linking,
    Linked,
    Evaluating,
    EvaluatingAsync,
    Evaluated,
}

/// `[[AsyncEvaluationOrder]]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsyncOrder {
    Unset,
    Order(u64),
    Done,
}

/// The fields of a Source Text Module Record (and of Cyclic Module
/// Records).
pub struct SourceTextModule {
    pub code: ModuleCode,
    pub requested: Vec<ModuleRequest>,
    pub import_entries: Vec<ImportEntry>,
    pub local_export_entries: Vec<ExportEntry>,
    pub indirect_export_entries: Vec<ExportEntry>,
    pub star_export_entries: Vec<ExportEntry>,
    pub dfs_index: u32,
    pub dfs_ancestor_index: u32,
    pub cycle_root: Option<Object>,
    pub async_order: AsyncOrder,
    pub async_parent_modules: Vec<Object>,
    pub pending_async_dependencies: u32,
    pub import_meta: Option<Object>,
    /// Slot of each binding of the module environment.
    slots: HashMap<String, u32>,
}

pub enum ModuleKind {
    Script,
    SourceText(Box<SourceTextModule>),
    /// A synthetic module whose only export, `default`, is a JSON value.
    Json,
}

/// A Script Record or Module Record.
pub struct ModuleRecord {
    pub realm: Realm,
    /// `[[HostDefined]]`: for the hosts of vidrio, the path or URL the
    /// record was loaded from.
    pub host_defined: Option<Rc<str>>,
    /// `[[LoadedModules]]`.
    pub loaded: Vec<(ModuleRequest, Object)>,
    pub kind: ModuleKind,
    pub status: ModuleStatus,
    pub env: Option<Env>,
    pub namespace: Option<Object>,
    pub eval_error: Option<Value>,
    pub top_level_capability: Option<Capability>,
}

impl Trace for ModuleRecord {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.realm(self.realm);
        for (_, m) in &self.loaded {
            tracer.object(*m);
        }
        if let Some(e) = self.env {
            tracer.env(e);
        }
        if let Some(n) = self.namespace {
            tracer.object(n);
        }
        if let Some(e) = self.eval_error {
            tracer.value(e);
        }
        if let Some(c) = &self.top_level_capability {
            c.trace(tracer);
        }
        if let ModuleKind::SourceText(st) = &self.kind {
            st.code.init.trace(tracer);
            st.code.body.trace(tracer);
            st.code.layout.trace(tracer);
            for o in st
                .cycle_root
                .iter()
                .chain(st.async_parent_modules.iter())
                .chain(st.import_meta.iter())
            {
                tracer.object(*o);
            }
        }
    }
}

/// Who asks for a module: a Script or Module Record, or a realm (an
/// `import()` with no active script or module).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Referrer {
    Record(Object),
    Realm(Realm),
}

/// The opaque state a host hands back to [`finish_loading_imported_module`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LoadPayload(u64);

/// The host hooks of modules. The browser implements them over fetch; the
/// `vjs` shell over the file system.
pub trait ModuleHost {
    /// HostLoadImportedModule(referrer, moduleRequest, hostDefined,
    /// payload). The host must call [`finish_loading_imported_module`]
    /// with the same arguments, synchronously or later, and must answer
    /// with the same record every time for the same referrer and request.
    fn load_imported_module(
        &self,
        agent: &mut Agent,
        referrer: Referrer,
        request: &ModuleRequest,
        payload: LoadPayload,
    );

    /// HostGetImportMetaProperties(moduleRecord).
    fn import_meta_properties(
        &self,
        _agent: &mut Agent,
        _module: Object,
    ) -> Vec<(PropertyKey, Value)> {
        Vec::new()
    }

    /// HostFinalizeImportMeta(importMeta, moduleRecord).
    fn finalize_import_meta(&self, _agent: &mut Agent, _meta: Object, _module: Object) {}

    /// HostGetSupportedImportAttributes().
    fn supported_import_attributes(&self) -> Vec<String> {
        vec!["type".to_string()]
    }
}

/// GraphLoadingState Record.
struct GraphLoadingState {
    capability: Capability,
    pending: u32,
    visited: Vec<Object>,
}

/// The agent's module state: every record, the pending graph loads and
/// dynamic imports, and the `[[LoadedModules]]` of realms.
#[derive(Default)]
pub struct ModuleState {
    records: Vec<Object>,
    graphs: HashMap<u64, GraphLoadingState>,
    dynamic: HashMap<u64, Capability>,
    next_payload: u64,
    realm_loaded: Vec<(Realm, ModuleRequest, Object)>,
    async_order: u64,
}

impl Trace for ModuleState {
    fn trace(&self, tracer: &mut Tracer) {
        for o in &self.records {
            tracer.object(*o);
        }
        for g in self.graphs.values() {
            g.capability.trace(tracer);
            for o in &g.visited {
                tracer.object(*o);
            }
        }
        for c in self.dynamic.values() {
            c.trace(tracer);
        }
        for (r, _, o) in &self.realm_loaded {
            tracer.realm(*r);
            tracer.object(*o);
        }
    }
}

fn rec(agent: &Agent, m: Object) -> &ModuleRecord {
    match &agent.heap.object(m).kind {
        ObjectKind::Module(r) => r,
        _ => panic!("not a module record"),
    }
}

fn rec_mut(agent: &mut Agent, m: Object) -> &mut ModuleRecord {
    match &mut agent.heap.object_mut(m).kind {
        ObjectKind::Module(r) => r,
        _ => panic!("not a module record"),
    }
}

fn st(agent: &Agent, m: Object) -> &SourceTextModule {
    match &rec(agent, m).kind {
        ModuleKind::SourceText(s) => s,
        _ => panic!("not a source text module"),
    }
}

fn st_mut(agent: &mut Agent, m: Object) -> &mut SourceTextModule {
    match &mut rec_mut(agent, m).kind {
        ModuleKind::SourceText(s) => s,
        _ => panic!("not a source text module"),
    }
}

/// Whether `o` is a Script or Module Record.
pub fn is_record(agent: &Agent, o: Object) -> bool {
    matches!(agent.heap.object(o).kind, ObjectKind::Module(_))
}

fn is_cyclic(agent: &Agent, m: Object) -> bool {
    matches!(rec(agent, m).kind, ModuleKind::SourceText(_))
}

fn status(agent: &Agent, m: Object) -> ModuleStatus {
    rec(agent, m).status
}

fn set_status(agent: &mut Agent, m: Object, s: ModuleStatus) {
    rec_mut(agent, m).status = s;
}

/// The `[[HostDefined]]` of a record.
pub fn host_defined(agent: &Agent, m: Object) -> Option<Rc<str>> {
    rec(agent, m).host_defined.clone()
}

/// The realm of a record.
pub fn record_realm(agent: &Agent, m: Object) -> Realm {
    rec(agent, m).realm
}

/// The `[[Status]]` of a module.
pub fn module_status(agent: &Agent, m: Object) -> ModuleStatus {
    status(agent, m)
}

/// The `[[EvaluationError]]` of a module.
pub fn evaluation_error(agent: &Agent, m: Object) -> Option<Value> {
    rec(agent, m).eval_error
}

fn alloc_record(agent: &mut Agent, record: ModuleRecord) -> Object {
    let o = agent
        .heap
        .create_object(None, ObjectKind::Module(Box::new(record)));
    agent.modules.records.push(o);
    o
}

// ----- creation -----

/// A Script Record for code evaluated by the host (`host_defined` is the
/// base for the `import()` calls of the script).
pub fn create_script_record(
    agent: &mut Agent,
    realm: Realm,
    host_defined: Option<Rc<str>>,
) -> Object {
    alloc_record(
        agent,
        ModuleRecord {
            realm,
            host_defined,
            loaded: Vec::new(),
            kind: ModuleKind::Script,
            status: ModuleStatus::Evaluated,
            env: None,
            namespace: None,
            eval_error: None,
            top_level_capability: None,
        },
    )
}

fn utf16_to_string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

fn ast_request(r: &crate::ast::ModuleRequest) -> ModuleRequest {
    ModuleRequest::new(
        utf16_to_string(&r.specifier),
        r.attributes
            .iter()
            .map(|(k, v)| (utf16_to_string(k), utf16_to_string(v)))
            .collect(),
    )
}

struct Entries {
    requested: Vec<ModuleRequest>,
    imports: Vec<ImportEntry>,
    local: Vec<ExportEntry>,
    indirect: Vec<ExportEntry>,
    star: Vec<ExportEntry>,
}

/// The ModuleRequests, ImportEntries and ExportEntries of a module
/// (ParseModule steps 4 to 11).
fn module_entries(program: &Program) -> Entries {
    fn index_of(r: &crate::ast::ModuleRequest, requested: &mut Vec<ModuleRequest>) -> usize {
        let req = ast_request(r);
        match requested.iter().position(|x| *x == req) {
            Some(i) => i,
            None => {
                requested.push(req);
                requested.len() - 1
            }
        }
    }
    let mut requested: Vec<ModuleRequest> = Vec::new();
    let mut imports = Vec::new();
    let mut exports: Vec<ExportEntry> = Vec::new();
    for s in &program.body {
        match &s.kind {
            StmtKind::Import(decl) => {
                let request = index_of(&decl.source, &mut requested);
                for spec in &decl.specifiers {
                    imports.push(match spec {
                        ImportSpecifier::Default(n) => ImportEntry {
                            request,
                            import_name: ImportName::Name("default".to_string()),
                            local_name: n.name.clone(),
                        },
                        ImportSpecifier::Namespace(n) => ImportEntry {
                            request,
                            import_name: ImportName::Namespace,
                            local_name: n.name.clone(),
                        },
                        ImportSpecifier::Named { imported, local } => ImportEntry {
                            request,
                            import_name: ImportName::Name(imported.name.clone()),
                            local_name: local.name.clone(),
                        },
                    });
                }
            }
            StmtKind::Export(decl) => match &**decl {
                ExportDecl::Named {
                    specifiers,
                    source: None,
                } => {
                    for spec in specifiers {
                        exports.push(ExportEntry {
                            export_name: Some(spec.exported.name.clone()),
                            request: None,
                            import_name: None,
                            local_name: Some(spec.local.name.clone()),
                        });
                    }
                }
                ExportDecl::Named {
                    specifiers,
                    source: Some(src),
                } => {
                    let request = index_of(src, &mut requested);
                    for spec in specifiers {
                        exports.push(ExportEntry {
                            export_name: Some(spec.exported.name.clone()),
                            request: Some(request),
                            import_name: Some(ExportImportName::Name(spec.local.name.clone())),
                            local_name: None,
                        });
                    }
                }
                ExportDecl::All { exported, source } => {
                    let request = index_of(source, &mut requested);
                    exports.push(match exported {
                        None => ExportEntry {
                            export_name: None,
                            request: Some(request),
                            import_name: Some(ExportImportName::AllButDefault),
                            local_name: None,
                        },
                        Some(e) => ExportEntry {
                            export_name: Some(e.name.clone()),
                            request: Some(request),
                            import_name: Some(ExportImportName::All),
                            local_name: None,
                        },
                    });
                }
                ExportDecl::Declaration(d) => {
                    let mut names = Vec::new();
                    match &d.kind {
                        StmtKind::Var(v) => {
                            for declarator in &v.declarations {
                                crate::compiler::bound_names(&declarator.target, &mut names);
                            }
                        }
                        StmtKind::Function(f) => {
                            names.extend(f.name.iter().map(|n| n.name.clone()))
                        }
                        StmtKind::Class(c) => names.extend(c.name.iter().map(|n| n.name.clone())),
                        _ => {}
                    }
                    for n in names {
                        exports.push(ExportEntry {
                            export_name: Some(n.clone()),
                            request: None,
                            import_name: None,
                            local_name: Some(n),
                        });
                    }
                }
                ExportDecl::DefaultFunction(f) => exports.push(ExportEntry {
                    export_name: Some("default".to_string()),
                    request: None,
                    import_name: None,
                    local_name: Some(
                        f.name
                            .as_ref()
                            .map_or(DEFAULT_LOCAL.to_string(), |n| n.name.clone()),
                    ),
                }),
                ExportDecl::DefaultClass(c) => exports.push(ExportEntry {
                    export_name: Some("default".to_string()),
                    request: None,
                    import_name: None,
                    local_name: Some(
                        c.name
                            .as_ref()
                            .map_or(DEFAULT_LOCAL.to_string(), |n| n.name.clone()),
                    ),
                }),
                ExportDecl::DefaultExpr(_) => exports.push(ExportEntry {
                    export_name: Some("default".to_string()),
                    request: None,
                    import_name: None,
                    local_name: Some(DEFAULT_LOCAL.to_string()),
                }),
            },
            _ => {}
        }
    }
    let mut local = Vec::new();
    let mut indirect = Vec::new();
    let mut star = Vec::new();
    for ee in exports {
        match ee.request {
            None => {
                let local_name = ee.local_name.clone().unwrap_or_default();
                match imports.iter().find(|ie| ie.local_name == local_name) {
                    None => local.push(ee),
                    Some(ie) => match &ie.import_name {
                        // A re-export of an imported namespace object.
                        ImportName::Namespace => local.push(ee),
                        ImportName::Name(n) => indirect.push(ExportEntry {
                            export_name: ee.export_name,
                            request: Some(ie.request),
                            import_name: Some(ExportImportName::Name(n.clone())),
                            local_name: None,
                        }),
                    },
                }
            }
            Some(_) if ee.import_name == Some(ExportImportName::AllButDefault) => star.push(ee),
            Some(_) => indirect.push(ee),
        }
    }
    Entries {
        requested,
        imports,
        local,
        indirect,
        star,
    }
}

/// ParseModule(sourceText, realm, hostDefined): a Source Text Module
/// Record with status `new`, or the early error.
pub fn parse_module_record(
    agent: &mut Agent,
    realm: Realm,
    source: &str,
    host_defined: Option<Rc<str>>,
) -> Result<Object, SyntaxError> {
    let program = parse_module(source, &ParseOptions::default())?;
    let entries = module_entries(&program);
    let code = compile_module(agent, &program, Rc::from(source));
    let slots = code
        .layout
        .names
        .iter()
        .enumerate()
        .map(|(i, a)| (agent.heap.atom_to_rust(*a), i as u32))
        .collect();
    let outer = global_env(agent, realm);
    let env = agent
        .heap
        .alloc_env_data(EnvData::for_scope(Some(outer), code.layout.clone()));
    Ok(alloc_record(
        agent,
        ModuleRecord {
            realm,
            host_defined,
            loaded: Vec::new(),
            kind: ModuleKind::SourceText(Box::new(SourceTextModule {
                code,
                requested: entries.requested,
                import_entries: entries.imports,
                local_export_entries: entries.local,
                indirect_export_entries: entries.indirect,
                star_export_entries: entries.star,
                dfs_index: 0,
                dfs_ancestor_index: 0,
                cycle_root: None,
                async_order: AsyncOrder::Unset,
                async_parent_modules: Vec::new(),
                pending_async_dependencies: 0,
                import_meta: None,
                slots,
            })),
            status: ModuleStatus::New,
            env: Some(env),
            namespace: None,
            eval_error: None,
            top_level_capability: None,
        },
    ))
}

/// ParseJSONModule(source): a synthetic module whose `default` export is
/// the parsed value; a malformed text throws a SyntaxError.
pub fn create_json_module(
    agent: &mut Agent,
    realm: Realm,
    source: &str,
    host_defined: Option<Rc<str>>,
) -> JsResult<Object> {
    let units: Vec<u16> = source.encode_utf16().collect();
    agent.push_context(crate::agent::ExecutionContext {
        realm,
        function: None,
        script_or_module: None,
    });
    let value = crate::json::parse_json(agent, &units);
    agent.pop_context();
    let value = value?;
    let default = agent.heap.intern_str("default");
    let layout = Rc::new(ScopeNames {
        names: vec![default],
        flags: vec![0],
        ..ScopeNames::default()
    });
    let outer = global_env(agent, realm);
    let mut data = EnvData::for_scope(Some(outer), layout);
    data.slots[0] = Some(value);
    let env = agent.heap.alloc_env_data(data);
    Ok(alloc_record(
        agent,
        ModuleRecord {
            realm,
            host_defined,
            loaded: Vec::new(),
            kind: ModuleKind::Json,
            status: ModuleStatus::New,
            env: Some(env),
            namespace: None,
            eval_error: None,
            top_level_capability: None,
        },
    ))
}

// ----- loading -----

fn new_payload(agent: &mut Agent) -> u64 {
    agent.modules.next_payload += 1;
    agent.modules.next_payload
}

/// GetImportedModule(referrer, request).
fn get_imported_module(agent: &Agent, referrer: Object, request: &ModuleRequest) -> Object {
    rec(agent, referrer)
        .loaded
        .iter()
        .find(|(r, _)| r == request)
        .map(|(_, m)| *m)
        .expect("the requested module was not loaded")
}

fn loaded_module(agent: &Agent, referrer: Referrer, request: &ModuleRequest) -> Option<Object> {
    match referrer {
        Referrer::Record(o) => rec(agent, o)
            .loaded
            .iter()
            .find(|(r, _)| r == request)
            .map(|(_, m)| *m),
        Referrer::Realm(realm) => agent
            .modules
            .realm_loaded
            .iter()
            .find(|(r, q, _)| *r == realm && q == request)
            .map(|(_, _, m)| *m),
    }
}

fn all_import_attributes_supported(agent: &Agent, request: &ModuleRequest) -> bool {
    let supported = match &agent.module_host {
        Some(h) => h.supported_import_attributes(),
        None => vec!["type".to_string()],
    };
    request
        .attributes
        .iter()
        .all(|(k, _)| supported.iter().any(|s| s == k))
}

fn host_load(agent: &mut Agent, referrer: Referrer, request: &ModuleRequest, payload: u64) {
    match agent.module_host.clone() {
        Some(host) => host.load_imported_module(agent, referrer, request, LoadPayload(payload)),
        None => {
            let e = agent.throw_error(crate::agent::ErrorKind::Type, "modules are not supported");
            finish_loading_imported_module(agent, referrer, request, LoadPayload(payload), Err(e));
        }
    }
}

/// LoadRequestedModules(): a promise that settles when the graph of
/// `module` is loaded.
pub fn load_requested_modules(agent: &mut Agent, module: Object) -> Value {
    let realm = agent.current_realm();
    let capability = promise::new_intrinsic_capability(agent, realm);
    let id = new_payload(agent);
    agent.modules.graphs.insert(
        id,
        GraphLoadingState {
            capability,
            pending: 1,
            visited: Vec::new(),
        },
    );
    inner_module_loading(agent, id, module);
    capability.promise
}

/// InnerModuleLoading(state, module).
fn inner_module_loading(agent: &mut Agent, id: u64, module: Object) {
    let Some(state) = agent.modules.graphs.get(&id) else {
        return;
    };
    let fresh = is_cyclic(agent, module)
        && status(agent, module) == ModuleStatus::New
        && !state.visited.contains(&module);
    if fresh {
        let requests = st(agent, module).requested.clone();
        if let Some(state) = agent.modules.graphs.get_mut(&id) {
            state.visited.push(module);
            state.pending += requests.len() as u32;
        }
        for request in &requests {
            if !all_import_attributes_supported(agent, request) {
                let e = agent.throw_error(
                    crate::agent::ErrorKind::Syntax,
                    "unsupported import attribute",
                );
                continue_module_loading(agent, id, Err(e));
            } else if let Some(m) = loaded_module(agent, Referrer::Record(module), request) {
                inner_module_loading(agent, id, m);
            } else {
                host_load(agent, Referrer::Record(module), request, id);
            }
            if !agent.modules.graphs.contains_key(&id) {
                return;
            }
        }
    }
    let done = match agent.modules.graphs.get_mut(&id) {
        Some(state) => {
            state.pending -= 1;
            state.pending == 0
        }
        None => return,
    };
    if done {
        let state = agent.modules.graphs.remove(&id).expect("graph state");
        for m in &state.visited {
            if status(agent, *m) == ModuleStatus::New {
                set_status(agent, *m, ModuleStatus::Unlinked);
            }
        }
        let _ = ops::call(
            agent,
            state.capability.resolve,
            Value::Undefined,
            &[Value::Undefined],
        );
    }
}

/// ContinueModuleLoading(state, moduleCompletion).
fn continue_module_loading(agent: &mut Agent, id: u64, result: Result<Object, Throw>) {
    if !agent.modules.graphs.contains_key(&id) {
        return;
    }
    match result {
        Ok(m) => inner_module_loading(agent, id, m),
        Err(Throw(e)) => {
            let state = agent.modules.graphs.remove(&id).expect("graph state");
            let _ = ops::call(agent, state.capability.reject, Value::Undefined, &[e]);
        }
    }
}

/// FinishLoadingImportedModule(referrer, moduleRequest, payload, result).
pub fn finish_loading_imported_module(
    agent: &mut Agent,
    referrer: Referrer,
    request: &ModuleRequest,
    payload: LoadPayload,
    result: Result<Object, Throw>,
) {
    if let Ok(m) = result {
        if loaded_module(agent, referrer, request).is_none() {
            match referrer {
                Referrer::Record(o) => rec_mut(agent, o).loaded.push((request.clone(), m)),
                Referrer::Realm(r) => agent.modules.realm_loaded.push((r, request.clone(), m)),
            }
        }
    }
    let id = payload.0;
    if agent.modules.graphs.contains_key(&id) {
        continue_module_loading(agent, id, result);
    } else if let Some(capability) = agent.modules.dynamic.remove(&id) {
        continue_dynamic_import(agent, capability, result);
    }
}

// ----- exports -----

/// The name a resolved export binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingName {
    Local(String),
    /// NAMESPACE: the namespace object of the module.
    Namespace,
}

/// The result of ResolveExport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Null,
    Ambiguous,
    Binding { module: Object, name: BindingName },
}

/// GetExportedNames(exportStarSet).
pub fn get_exported_names(
    agent: &Agent,
    module: Object,
    star_set: &mut Vec<Object>,
) -> Vec<String> {
    let st = match &rec(agent, module).kind {
        ModuleKind::SourceText(s) => s,
        ModuleKind::Json => return vec!["default".to_string()],
        ModuleKind::Script => return Vec::new(),
    };
    if star_set.contains(&module) {
        return Vec::new();
    }
    star_set.push(module);
    let mut names: Vec<String> = st
        .local_export_entries
        .iter()
        .chain(st.indirect_export_entries.iter())
        .filter_map(|e| e.export_name.clone())
        .collect();
    for e in &st.star_export_entries {
        let request = &st.requested[e.request.expect("star export request")];
        let requested = get_imported_module(agent, module, request);
        for n in get_exported_names(agent, requested, star_set) {
            if n != "default" && !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names
}

/// ResolveExport(exportName, resolveSet).
pub fn resolve_export(
    agent: &Agent,
    module: Object,
    export_name: &str,
    resolve_set: &mut Vec<(Object, String)>,
) -> Resolution {
    let st = match &rec(agent, module).kind {
        ModuleKind::SourceText(s) => s,
        ModuleKind::Json => {
            return if export_name == "default" {
                Resolution::Binding {
                    module,
                    name: BindingName::Local("default".to_string()),
                }
            } else {
                Resolution::Null
            };
        }
        ModuleKind::Script => return Resolution::Null,
    };
    if resolve_set
        .iter()
        .any(|(m, n)| *m == module && n == export_name)
    {
        // A circular import request.
        return Resolution::Null;
    }
    resolve_set.push((module, export_name.to_string()));
    for e in &st.local_export_entries {
        if e.export_name.as_deref() == Some(export_name) {
            return Resolution::Binding {
                module,
                name: BindingName::Local(e.local_name.clone().unwrap_or_default()),
            };
        }
    }
    for e in &st.indirect_export_entries {
        if e.export_name.as_deref() == Some(export_name) {
            let request = &st.requested[e.request.expect("indirect export request")];
            let imported = get_imported_module(agent, module, request);
            return match &e.import_name {
                Some(ExportImportName::All) => Resolution::Binding {
                    module: imported,
                    name: BindingName::Namespace,
                },
                Some(ExportImportName::Name(n)) => resolve_export(agent, imported, n, resolve_set),
                _ => Resolution::Null,
            };
        }
    }
    if export_name == "default" {
        return Resolution::Null;
    }
    let mut star_resolution = Resolution::Null;
    for e in &st.star_export_entries {
        let request = &st.requested[e.request.expect("star export request")];
        let imported = get_imported_module(agent, module, request);
        let resolution = resolve_export(agent, imported, export_name, resolve_set);
        match resolution {
            Resolution::Ambiguous => return Resolution::Ambiguous,
            Resolution::Null => {}
            Resolution::Binding { .. } => {
                if star_resolution == Resolution::Null {
                    star_resolution = resolution;
                } else if star_resolution != resolution {
                    // A different module or a different binding name.
                    return Resolution::Ambiguous;
                }
            }
        }
    }
    star_resolution
}

/// The environment and slot of a local binding of a module.
fn binding_slot(agent: &Agent, module: Object, name: &str) -> Option<(Env, u32)> {
    let r = rec(agent, module);
    let env = r.env?;
    match &r.kind {
        ModuleKind::SourceText(s) => s.slots.get(name).map(|&slot| (env, slot)),
        ModuleKind::Json => (name == "default").then_some((env, 0)),
        ModuleKind::Script => None,
    }
}

/// GetModuleNamespace(module).
pub fn get_module_namespace(agent: &mut Agent, module: Object) -> Object {
    if let Some(ns) = rec(agent, module).namespace {
        return ns;
    }
    let names = get_exported_names(agent, module, &mut Vec::new());
    let mut exports = Vec::new();
    let mut namespaces = Vec::new();
    for name in names {
        let resolution = resolve_export(agent, module, &name, &mut Vec::new());
        let Resolution::Binding {
            module: target,
            name: binding,
        } = resolution
        else {
            continue;
        };
        let key = agent.heap.key_from_str(&name);
        match binding {
            BindingName::Namespace => {
                namespaces.push((key, target));
                // Patched below, once this namespace exists (the target
                // may be this module).
                exports.push((key, NamespaceBinding::Namespace(target)));
            }
            BindingName::Local(local) => {
                if let Some((env, slot)) = binding_slot(agent, target, &local) {
                    exports.push((key, NamespaceBinding::Slot { env, slot }));
                }
            }
        }
    }
    let ns = module_namespace_create(agent, Some(module), exports);
    rec_mut(agent, module).namespace = Some(ns);
    for (key, target) in namespaces {
        let target_ns = get_module_namespace(agent, target);
        if let ObjectKind::ModuleNamespace(data) = &mut agent.heap.object_mut(ns).kind {
            if let Some(&i) = data.index.get(&key) {
                data.exports[i].1 = NamespaceBinding::Namespace(target_ns);
            }
        }
    }
    ns
}

// ----- linking -----

fn request_specifier(agent: &Agent, module: Object, request: usize) -> String {
    st(agent, module).requested[request].specifier.clone()
}

/// Link(): links the graph of `module` (its status must be at least
/// `unlinked`).
pub fn link(agent: &mut Agent, module: Object) -> JsResult<()> {
    let mut stack = Vec::new();
    let result = inner_module_linking(agent, module, &mut stack, 0);
    if let Err(e) = result {
        for m in stack {
            set_status(agent, m, ModuleStatus::Unlinked);
        }
        return Err(e);
    }
    Ok(())
}

/// InnerModuleLinking(module, stack, index).
fn inner_module_linking(
    agent: &mut Agent,
    module: Object,
    stack: &mut Vec<Object>,
    index: u32,
) -> JsResult<u32> {
    if !is_cyclic(agent, module) {
        if status(agent, module) != ModuleStatus::Evaluated {
            set_status(agent, module, ModuleStatus::Linked);
        }
        return Ok(index);
    }
    match status(agent, module) {
        ModuleStatus::Linking
        | ModuleStatus::Linked
        | ModuleStatus::EvaluatingAsync
        | ModuleStatus::Evaluated => return Ok(index),
        ModuleStatus::Unlinked => {}
        s => return agent.type_error(&format!("cannot link a module in state {s:?}")),
    }
    set_status(agent, module, ModuleStatus::Linking);
    {
        let s = st_mut(agent, module);
        s.dfs_index = index;
        s.dfs_ancestor_index = index;
    }
    let mut index = index + 1;
    stack.push(module);
    let requests = st(agent, module).requested.clone();
    for request in &requests {
        let required = get_imported_module(agent, module, request);
        index = inner_module_linking(agent, required, stack, index)?;
        if is_cyclic(agent, required) && status(agent, required) == ModuleStatus::Linking {
            let ancestor = st(agent, required).dfs_ancestor_index;
            let s = st_mut(agent, module);
            s.dfs_ancestor_index = s.dfs_ancestor_index.min(ancestor);
        }
    }
    initialize_environment(agent, module)?;
    let s = st(agent, module);
    if s.dfs_ancestor_index == s.dfs_index {
        while let Some(m) = stack.pop() {
            set_status(agent, m, ModuleStatus::Linked);
            if m == module {
                break;
            }
        }
    }
    Ok(index)
}

fn unresolved_error<T>(
    agent: &mut Agent,
    specifier: &str,
    name: &str,
    resolution: &Resolution,
) -> JsResult<T> {
    let msg = if *resolution == Resolution::Ambiguous {
        format!("the export '{name}' of module '{specifier}' is ambiguous")
    } else {
        format!("the module '{specifier}' does not provide an export named '{name}'")
    };
    agent.syntax_error(&msg)
}

/// InitializeEnvironment() of a Source Text Module Record.
fn initialize_environment(agent: &mut Agent, module: Object) -> JsResult<()> {
    let indirect = st(agent, module).indirect_export_entries.clone();
    for e in &indirect {
        let name = e.export_name.clone().unwrap_or_default();
        let resolution = resolve_export(agent, module, &name, &mut Vec::new());
        if !matches!(resolution, Resolution::Binding { .. }) {
            let specifier = request_specifier(agent, module, e.request.unwrap_or(0));
            let import_name = match &e.import_name {
                Some(ExportImportName::Name(n)) => n.clone(),
                _ => name.clone(),
            };
            return unresolved_error(agent, &specifier, &import_name, &resolution);
        }
    }
    let env = rec(agent, module).env.expect("module environment");
    let imports = st(agent, module).import_entries.clone();
    let requested = st(agent, module).requested.clone();
    for ie in &imports {
        let imported = get_imported_module(agent, module, &requested[ie.request]);
        let slot = st(agent, module).slots[&ie.local_name];
        match &ie.import_name {
            ImportName::Namespace => {
                let ns = get_module_namespace(agent, imported);
                agent.heap.env_mut(env).slots[slot as usize] = Some(Value::Object(ns));
            }
            ImportName::Name(name) => {
                let resolution = resolve_export(agent, imported, name, &mut Vec::new());
                match resolution {
                    Resolution::Binding {
                        module: target,
                        name: BindingName::Namespace,
                    } => {
                        let ns = get_module_namespace(agent, target);
                        agent.heap.env_mut(env).slots[slot as usize] = Some(Value::Object(ns));
                    }
                    Resolution::Binding {
                        module: target,
                        name: BindingName::Local(local),
                    } => {
                        let Some((target_env, target_slot)) = binding_slot(agent, target, &local)
                        else {
                            let specifier = requested[ie.request].specifier.clone();
                            return unresolved_error(agent, &specifier, name, &Resolution::Null);
                        };
                        let data = agent.heap.env_mut(env);
                        let imports = data.imports.get_or_insert_with(Default::default);
                        imports.retain(|(s, _, _)| *s != slot);
                        imports.push((slot, target_env, target_slot));
                    }
                    r => {
                        let specifier = requested[ie.request].specifier.clone();
                        return unresolved_error(agent, &specifier, name, &r);
                    }
                }
            }
        }
    }
    // The hoisted function declarations.
    let code = st(agent, module).code.init.clone();
    let frame = module_frame(agent, module, code);
    run(agent, frame)?;
    Ok(())
}

fn module_frame(agent: &Agent, module: Object, code: Rc<crate::bytecode::Code>) -> Frame {
    let r = rec(agent, module);
    let mut frame = Frame::new(
        code,
        r.env.expect("module environment"),
        r.realm,
        FrameKind::Script,
    );
    frame.this = Some(Value::Undefined);
    frame.script_or_module = Some(module);
    frame
}

// ----- evaluation -----

/// Evaluate(): evaluates the graph of a linked `module` and returns the
/// promise of its evaluation.
pub fn evaluate(agent: &mut Agent, module: Object) -> Value {
    let realm = agent.current_realm();
    if !is_cyclic(agent, module) {
        // A synthetic module evaluates synchronously.
        set_status(agent, module, ModuleStatus::Evaluated);
        let cap = promise::new_intrinsic_capability(agent, realm);
        let _ = ops::call(agent, cap.resolve, Value::Undefined, &[Value::Undefined]);
        return cap.promise;
    }
    let mut module = module;
    if matches!(
        status(agent, module),
        ModuleStatus::EvaluatingAsync | ModuleStatus::Evaluated
    ) {
        if let Some(root) = st(agent, module).cycle_root {
            module = root;
        }
    }
    if let Some(cap) = rec(agent, module).top_level_capability {
        return cap.promise;
    }
    let cap = promise::new_intrinsic_capability(agent, realm);
    rec_mut(agent, module).top_level_capability = Some(cap);
    let mut stack = Vec::new();
    match inner_module_evaluation(agent, module, &mut stack, 0) {
        Err(Throw(e)) => {
            for m in stack {
                set_status(agent, m, ModuleStatus::Evaluated);
                rec_mut(agent, m).eval_error = Some(e);
            }
            let _ = ops::call(agent, cap.reject, Value::Undefined, &[e]);
        }
        Ok(_) => {
            if st(agent, module).async_order == AsyncOrder::Unset {
                let _ = ops::call(agent, cap.resolve, Value::Undefined, &[Value::Undefined]);
            }
        }
    }
    cap.promise
}

/// InnerModuleEvaluation(module, stack, index).
fn inner_module_evaluation(
    agent: &mut Agent,
    module: Object,
    stack: &mut Vec<Object>,
    index: u32,
) -> JsResult<u32> {
    if !is_cyclic(agent, module) {
        set_status(agent, module, ModuleStatus::Evaluated);
        return Ok(index);
    }
    match status(agent, module) {
        ModuleStatus::EvaluatingAsync | ModuleStatus::Evaluated => {
            return match rec(agent, module).eval_error {
                None => Ok(index),
                Some(e) => Err(Throw(e)),
            };
        }
        ModuleStatus::Evaluating => return Ok(index),
        ModuleStatus::Linked => {}
        s => return agent.type_error(&format!("cannot evaluate a module in state {s:?}")),
    }
    set_status(agent, module, ModuleStatus::Evaluating);
    {
        let s = st_mut(agent, module);
        s.dfs_index = index;
        s.dfs_ancestor_index = index;
        s.pending_async_dependencies = 0;
    }
    let mut index = index + 1;
    stack.push(module);
    let requests = st(agent, module).requested.clone();
    for request in &requests {
        let mut required = get_imported_module(agent, module, request);
        index = inner_module_evaluation(agent, required, stack, index)?;
        if !is_cyclic(agent, required) {
            continue;
        }
        if status(agent, required) == ModuleStatus::Evaluating {
            let ancestor = st(agent, required).dfs_ancestor_index;
            let s = st_mut(agent, module);
            s.dfs_ancestor_index = s.dfs_ancestor_index.min(ancestor);
        } else {
            required = st(agent, required).cycle_root.unwrap_or(required);
            if let Some(e) = rec(agent, required).eval_error {
                return Err(Throw(e));
            }
        }
        if matches!(st(agent, required).async_order, AsyncOrder::Order(_)) {
            st_mut(agent, module).pending_async_dependencies += 1;
            st_mut(agent, required).async_parent_modules.push(module);
        }
    }
    let (pending, has_tla) = {
        let s = st(agent, module);
        (s.pending_async_dependencies, s.code.has_tla)
    };
    if pending > 0 || has_tla {
        agent.modules.async_order += 1;
        let order = agent.modules.async_order;
        st_mut(agent, module).async_order = AsyncOrder::Order(order);
        if pending == 0 {
            execute_async_module(agent, module);
        }
    } else {
        execute_module_sync(agent, module)?;
    }
    let s = st(agent, module);
    if s.dfs_ancestor_index == s.dfs_index {
        while let Some(m) = stack.pop() {
            let next = if st(agent, m).async_order == AsyncOrder::Unset {
                ModuleStatus::Evaluated
            } else {
                ModuleStatus::EvaluatingAsync
            };
            set_status(agent, m, next);
            st_mut(agent, m).cycle_root = Some(module);
            if m == module {
                break;
            }
        }
    }
    Ok(index)
}

/// ExecuteModule() of a module without top-level `await`.
fn execute_module_sync(agent: &mut Agent, module: Object) -> JsResult<()> {
    let code = st(agent, module).code.body.clone();
    let frame = module_frame(agent, module, code);
    run(agent, frame)?;
    Ok(())
}

/// ExecuteAsyncModule(module).
fn execute_async_module(agent: &mut Agent, module: Object) {
    let realm = rec(agent, module).realm;
    let cap = promise::new_intrinsic_capability(agent, realm);
    let on_fulfilled = create_native_function(
        agent,
        async_module_fulfilled_behavior,
        0,
        "",
        vec![Value::Object(module)],
        None,
    );
    let on_rejected = create_native_function(
        agent,
        async_module_rejected_behavior,
        1,
        "",
        vec![Value::Object(module)],
        None,
    );
    if let Value::Object(p) = cap.promise {
        promise::perform_promise_then(
            agent,
            p,
            Value::Object(on_fulfilled),
            Value::Object(on_rejected),
            None,
        );
    }
    let code = st(agent, module).code.body.clone();
    let mut frame = module_frame(agent, module, code);
    frame.async_cap = Some(cap);
    if let Err(Throw(e)) = run(agent, frame) {
        // The body could not start (call stack exhausted).
        let _ = ops::call(agent, cap.reject, Value::Undefined, &[e]);
    }
}

fn async_module_fulfilled_behavior(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    if let Some(Value::Object(m)) = native_slots(agent, call.callee).first().copied() {
        async_module_execution_fulfilled(agent, m);
    }
    Ok(Value::Undefined)
}

fn async_module_rejected_behavior(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    if let Some(Value::Object(m)) = native_slots(agent, call.callee).first().copied() {
        async_module_execution_rejected(agent, m, call.arg(0));
    }
    Ok(Value::Undefined)
}

/// GatherAvailableAncestors(module, execList).
fn gather_available_ancestors(agent: &mut Agent, module: Object, exec_list: &mut Vec<Object>) {
    let parents = st(agent, module).async_parent_modules.clone();
    for m in parents {
        if exec_list.contains(&m) {
            continue;
        }
        let root = st(agent, m).cycle_root.unwrap_or(m);
        if rec(agent, root).eval_error.is_some() {
            continue;
        }
        let s = st_mut(agent, m);
        s.pending_async_dependencies = s.pending_async_dependencies.saturating_sub(1);
        if s.pending_async_dependencies == 0 {
            let has_tla = s.code.has_tla;
            exec_list.push(m);
            if !has_tla {
                gather_available_ancestors(agent, m, exec_list);
            }
        }
    }
}

fn async_order_key(agent: &Agent, m: Object) -> u64 {
    match st(agent, m).async_order {
        AsyncOrder::Order(n) => n,
        _ => u64::MAX,
    }
}

/// AsyncModuleExecutionFulfilled(module).
fn async_module_execution_fulfilled(agent: &mut Agent, module: Object) {
    if status(agent, module) == ModuleStatus::Evaluated {
        return;
    }
    st_mut(agent, module).async_order = AsyncOrder::Done;
    set_status(agent, module, ModuleStatus::Evaluated);
    if let Some(cap) = rec(agent, module).top_level_capability {
        let _ = ops::call(agent, cap.resolve, Value::Undefined, &[Value::Undefined]);
    }
    let mut exec_list = Vec::new();
    gather_available_ancestors(agent, module, &mut exec_list);
    let ordered: &Agent = agent;
    exec_list.sort_by_key(|m| async_order_key(ordered, *m));
    for m in exec_list {
        if status(agent, m) == ModuleStatus::Evaluated {
            continue;
        }
        if st(agent, m).code.has_tla {
            execute_async_module(agent, m);
            continue;
        }
        match execute_module_sync(agent, m) {
            Err(Throw(e)) => async_module_execution_rejected(agent, m, e),
            Ok(()) => {
                st_mut(agent, m).async_order = AsyncOrder::Done;
                set_status(agent, m, ModuleStatus::Evaluated);
                if let Some(cap) = rec(agent, m).top_level_capability {
                    let _ = ops::call(agent, cap.resolve, Value::Undefined, &[Value::Undefined]);
                }
            }
        }
    }
}

/// AsyncModuleExecutionRejected(module, error).
fn async_module_execution_rejected(agent: &mut Agent, module: Object, error: Value) {
    if status(agent, module) == ModuleStatus::Evaluated {
        return;
    }
    rec_mut(agent, module).eval_error = Some(error);
    set_status(agent, module, ModuleStatus::Evaluated);
    st_mut(agent, module).async_order = AsyncOrder::Done;
    let parents = st(agent, module).async_parent_modules.clone();
    for m in parents {
        async_module_execution_rejected(agent, m, error);
    }
    if let Some(cap) = rec(agent, module).top_level_capability {
        let _ = ops::call(agent, cap.reject, Value::Undefined, &[error]);
    }
}

// ----- import() and import.meta -----

fn reject_with(agent: &mut Agent, cap: Capability, e: Value) -> Value {
    let _ = ops::call(agent, cap.reject, Value::Undefined, &[e]);
    cap.promise
}

/// The referrer of `import()` at the current position.
fn current_referrer(agent: &Agent) -> Referrer {
    match agent.active_script_or_module() {
        Some(o) => Referrer::Record(o),
        None => Referrer::Realm(agent.current_realm()),
    }
}

/// EvaluateImportCall: the promise of `import(specifier, options)`.
pub fn evaluate_import_call(
    agent: &mut Agent,
    specifier: Value,
    options: Value,
) -> JsResult<Value> {
    let referrer = current_referrer(agent);
    let realm = agent.current_realm();
    let cap = promise::new_intrinsic_capability(agent, realm);
    let specifier = match ops::to_string(agent, specifier) {
        Ok(s) => agent.heap.string_to_rust(s),
        Err(Throw(e)) => return Ok(reject_with(agent, cap, e)),
    };
    let mut attributes = Vec::new();
    if !options.is_undefined() {
        let Value::Object(o) = options else {
            let e = agent.throw_error(
                crate::agent::ErrorKind::Type,
                "the options of import() must be an object",
            );
            return Ok(reject_with(agent, cap, e.0));
        };
        let with_key = agent.heap.key_from_str("with");
        let with = match ops::get(agent, o, with_key) {
            Ok(v) => v,
            Err(Throw(e)) => return Ok(reject_with(agent, cap, e)),
        };
        if !with.is_undefined() {
            let Value::Object(w) = with else {
                let e = agent.throw_error(
                    crate::agent::ErrorKind::Type,
                    "the 'with' option of import() must be an object",
                );
                return Ok(reject_with(agent, cap, e.0));
            };
            let entries = match ops::enumerable_own_properties(agent, w, ops::EnumerableKind::Key) {
                Ok(v) => v,
                Err(Throw(e)) => return Ok(reject_with(agent, cap, e)),
            };
            for key in entries {
                let Value::String(k) = key else { continue };
                let pk = agent.heap.key_from_string(k);
                let value = match ops::get(agent, w, pk) {
                    Ok(v) => v,
                    Err(Throw(e)) => return Ok(reject_with(agent, cap, e)),
                };
                let Value::String(v) = value else {
                    let e = agent.throw_error(
                        crate::agent::ErrorKind::Type,
                        "import attribute values must be strings",
                    );
                    return Ok(reject_with(agent, cap, e.0));
                };
                let k = agent.heap.string_to_rust(k);
                let v = agent.heap.string_to_rust(v);
                attributes.push((k, v));
            }
        }
    }
    let request = ModuleRequest::new(specifier, attributes);
    if !all_import_attributes_supported(agent, &request) {
        let e = agent.throw_error(
            crate::agent::ErrorKind::Syntax,
            "unsupported import attribute",
        );
        return Ok(reject_with(agent, cap, e.0));
    }
    let id = new_payload(agent);
    agent.modules.dynamic.insert(id, cap);
    match loaded_module(agent, referrer, &request) {
        Some(m) => {
            finish_loading_imported_module(agent, referrer, &request, LoadPayload(id), Ok(m))
        }
        None => host_load(agent, referrer, &request, id),
    }
    Ok(cap.promise)
}

/// ContinueDynamicImport(promiseCapability, moduleCompletion).
fn continue_dynamic_import(agent: &mut Agent, cap: Capability, result: Result<Object, Throw>) {
    let module = match result {
        Ok(m) => m,
        Err(Throw(e)) => {
            reject_with(agent, cap, e);
            return;
        }
    };
    let load_promise = load_requested_modules(agent, module);
    let slots = vec![cap.promise, cap.resolve, cap.reject, Value::Object(module)];
    let on_rejected =
        create_native_function(agent, dynamic_import_rejected, 1, "", slots.clone(), None);
    let link_and_evaluate =
        create_native_function(agent, dynamic_import_link_and_evaluate, 0, "", slots, None);
    if let Value::Object(p) = load_promise {
        promise::perform_promise_then(
            agent,
            p,
            Value::Object(link_and_evaluate),
            Value::Object(on_rejected),
            None,
        );
    }
}

fn slot_capability(agent: &Agent, f: Object) -> (Capability, Value) {
    let s = native_slots(agent, f);
    (
        Capability {
            promise: s[0],
            resolve: s[1],
            reject: s[2],
        },
        s[3],
    )
}

fn dynamic_import_rejected(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (cap, _) = slot_capability(agent, call.callee);
    reject_with(agent, cap, call.arg(0));
    Ok(Value::Undefined)
}

fn dynamic_import_link_and_evaluate(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (cap, module) = slot_capability(agent, call.callee);
    let Value::Object(module) = module else {
        return Ok(Value::Undefined);
    };
    if let Err(Throw(e)) = link(agent, module) {
        reject_with(agent, cap, e);
        return Ok(Value::Undefined);
    }
    let evaluate_promise = evaluate(agent, module);
    let slots = vec![cap.promise, cap.resolve, cap.reject, Value::Object(module)];
    let on_fulfilled =
        create_native_function(agent, dynamic_import_fulfilled, 0, "", slots.clone(), None);
    let on_rejected = create_native_function(agent, dynamic_import_rejected, 1, "", slots, None);
    if let Value::Object(p) = evaluate_promise {
        promise::perform_promise_then(
            agent,
            p,
            Value::Object(on_fulfilled),
            Value::Object(on_rejected),
            None,
        );
    }
    Ok(Value::Undefined)
}

fn dynamic_import_fulfilled(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (cap, module) = slot_capability(agent, call.callee);
    if let Value::Object(module) = module {
        let ns = get_module_namespace(agent, module);
        let _ = ops::call(agent, cap.resolve, Value::Undefined, &[Value::Object(ns)]);
    }
    Ok(Value::Undefined)
}

/// The `import.meta` object of the active module (created on first use
/// with the host's properties).
pub fn import_meta(agent: &mut Agent) -> JsResult<Object> {
    let Some(module) = agent.active_script_or_module() else {
        return agent.syntax_error("import.meta is only valid in module code");
    };
    if !is_cyclic(agent, module) {
        return agent.syntax_error("import.meta is only valid in module code");
    }
    if let Some(meta) = st(agent, module).import_meta {
        return Ok(meta);
    }
    let meta = agent.heap.create_object(None, ObjectKind::Ordinary);
    st_mut(agent, module).import_meta = Some(meta);
    if let Some(host) = agent.module_host.clone() {
        for (key, value) in host.import_meta_properties(agent, module) {
            ops::create_data_property_or_throw(agent, meta, key, value)?;
        }
        host.finalize_import_meta(agent, meta, module);
    }
    Ok(meta)
}

// ----- convenience for hosts -----

/// Loads, links and evaluates `module` (a root of the host, such as the
/// module script of a page): the promise of its evaluation. A failure to
/// load or link rejects it.
pub fn load_link_evaluate(agent: &mut Agent, module: Object) -> Value {
    let realm = rec(agent, module).realm;
    let cap = promise::new_intrinsic_capability(agent, realm);
    let load_promise = load_requested_modules(agent, module);
    let Value::Object(lp) = load_promise else {
        return cap.promise;
    };
    match promise::promise_state(agent, lp) {
        Some((promise::PromiseState::Rejected, e)) => return reject_with(agent, cap, e),
        Some((promise::PromiseState::Fulfilled, _)) => {}
        _ => {
            // Asynchronous host: continue when the graph is loaded.
            let slots = vec![cap.promise, cap.resolve, cap.reject, Value::Object(module)];
            let on_rejected =
                create_native_function(agent, dynamic_import_rejected, 1, "", slots.clone(), None);
            let next = create_native_function(agent, root_link_and_evaluate, 0, "", slots, None);
            promise::perform_promise_then(
                agent,
                lp,
                Value::Object(next),
                Value::Object(on_rejected),
                None,
            );
            return cap.promise;
        }
    }
    if let Err(Throw(e)) = link(agent, module) {
        return reject_with(agent, cap, e);
    }
    evaluate(agent, module)
}

fn root_link_and_evaluate(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (cap, module) = slot_capability(agent, call.callee);
    let Value::Object(module) = module else {
        return Ok(Value::Undefined);
    };
    if let Err(Throw(e)) = link(agent, module) {
        reject_with(agent, cap, e);
        return Ok(Value::Undefined);
    }
    let p = evaluate(agent, module);
    if let Value::Object(p) = p {
        promise::perform_promise_then(agent, p, cap.resolve, cap.reject, None);
    }
    Ok(Value::Undefined)
}
