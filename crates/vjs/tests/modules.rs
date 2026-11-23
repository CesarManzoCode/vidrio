//! Module tests over an in-memory host: linking and live bindings, cycles
//! (with functions hoisted across them), ambiguous star exports, top-level
//! `await` and the order of asynchronous evaluation, `import()`, JSON
//! modules, `import.meta` and namespace objects. Modules write what they
//! observe into globals that the tests read back.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vjs::agent::{Agent, ErrorKind};
use vjs::gc::{Object, Realm};
use vjs::interp::promise::{PromiseState, promise_state};
use vjs::module::{self, LoadPayload, ModuleHost, ModuleRequest, Referrer};
use vjs::{Throw, Value, evaluate_script, ops};

/// Modules by specifier; one record per specifier and module type.
struct MemHost {
    files: HashMap<String, String>,
    map: RefCell<HashMap<(String, bool), Object>>,
    loads: RefCell<Vec<String>>,
}

impl MemHost {
    fn new(files: &[(&str, &str)]) -> MemHost {
        MemHost {
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            map: RefCell::new(HashMap::new()),
            loads: RefCell::new(Vec::new()),
        }
    }

    fn get(
        &self,
        agent: &mut Agent,
        realm: Realm,
        request: &ModuleRequest,
    ) -> Result<Object, Throw> {
        let json = request.module_type() == Some("json");
        let key = (request.specifier.clone(), json);
        if let Some(m) = self.map.borrow().get(&key) {
            return Ok(*m);
        }
        self.loads.borrow_mut().push(request.specifier.clone());
        let Some(source) = self.files.get(&request.specifier).cloned() else {
            return Err(agent.throw_error(ErrorKind::Type, "no such module"));
        };
        let m = if json {
            module::create_json_module(agent, realm, &source, None)?
        } else {
            module::parse_module_record(agent, realm, &source, None)
                .map_err(|e| agent.throw_error(ErrorKind::Syntax, &e.message))?
        };
        self.map.borrow_mut().insert(key, m);
        Ok(m)
    }
}

impl ModuleHost for MemHost {
    fn load_imported_module(
        &self,
        agent: &mut Agent,
        referrer: Referrer,
        request: &ModuleRequest,
        payload: LoadPayload,
    ) {
        let realm = match referrer {
            Referrer::Record(o) => module::record_realm(agent, o),
            Referrer::Realm(r) => r,
        };
        let result = self.get(agent, realm, request);
        module::finish_loading_imported_module(agent, referrer, request, payload, result);
    }

    fn import_meta_properties(
        &self,
        agent: &mut Agent,
        _module: Object,
    ) -> Vec<(vjs::PropertyKey, Value)> {
        let key = agent.heap.key_from_str("url");
        let value = Value::String(agent.heap.alloc_str("mem:test"));
        vec![(key, value)]
    }
}

struct Run {
    agent: Agent,
    promise: Value,
}

fn setup(files: &[(&str, &str)]) -> (Agent, Rc<MemHost>, Realm) {
    let mut agent = Agent::new();
    let realm = agent.initialize_host_defined_realm();
    let host = Rc::new(MemHost::new(files));
    agent.module_host = Some(host.clone() as Rc<dyn ModuleHost>);
    (agent, host, realm)
}

fn drain(agent: &mut Agent) {
    while agent.has_pending_jobs() {
        agent.run_jobs();
    }
}

/// Loads, links and evaluates the module `root` and drains the jobs.
fn run(files: &[(&str, &str)], root: &str) -> Run {
    let (mut agent, host, realm) = setup(files);
    let request = ModuleRequest::new(root.to_string(), Vec::new());
    let m = host.get(&mut agent, realm, &request).expect("root module");
    let promise = module::load_link_evaluate(&mut agent, m);
    drain(&mut agent);
    Run { agent, promise }
}

fn global(agent: &mut Agent, name: &str) -> String {
    let g = agent.global_object();
    let key = agent.heap.key_from_str(name);
    let v = ops::get(agent, g, key).unwrap();
    let s = ops::to_string(agent, v).unwrap();
    agent.heap.string_to_rust(s)
}

fn state(r: &mut Run) -> (PromiseState, String) {
    let Value::Object(p) = r.promise else {
        panic!("not a promise");
    };
    let (s, v) = promise_state(&r.agent, p).unwrap();
    let text = match v {
        Value::Object(o) => {
            let key = r.agent.heap.key_from_str("name");
            let name = ops::get(&mut r.agent, o, key).unwrap();
            let name = ops::to_string(&mut r.agent, name).unwrap();
            r.agent.heap.string_to_rust(name)
        }
        v => {
            let s = ops::to_string(&mut r.agent, v).unwrap();
            r.agent.heap.string_to_rust(s)
        }
    };
    (s, text)
}

#[test]
fn imports_are_live_bindings() {
    let mut r = run(
        &[
            (
                "main",
                "import { count, inc } from 'counter';
                 import def, * as ns from 'counter';
                 globalThis.before = count;
                 inc(); inc();
                 globalThis.after = count + ',' + ns.count + ',' + def;
                 try { count = 5; } catch (e) { globalThis.assign = e.constructor.name; }",
            ),
            (
                "counter",
                "export let count = 0;
                 export function inc() { count++; }
                 export default 'dflt';",
            ),
        ],
        "main",
    );
    assert_eq!(state(&mut r).0, PromiseState::Fulfilled);
    assert_eq!(global(&mut r.agent, "before"), "0");
    assert_eq!(global(&mut r.agent, "after"), "2,2,dflt");
    assert_eq!(global(&mut r.agent, "assign"), "TypeError");
}

#[test]
fn renamed_and_reexported_bindings() {
    let mut r = run(
        &[
            (
                "main",
                "import { a as x, b, ns } from 'mid';
                 globalThis.out = x + b + ns.v;",
            ),
            (
                "mid",
                "export { v as a } from 'leaf';
                 import { v } from 'leaf';
                 export { v as b };
                 export * as ns from 'leaf';",
            ),
            ("leaf", "export var v = 'L';"),
        ],
        "main",
    );
    assert_eq!(state(&mut r).0, PromiseState::Fulfilled);
    assert_eq!(global(&mut r.agent, "out"), "LLL");
}

#[test]
fn cycles_see_hoisted_functions_and_tdz() {
    let mut r = run(
        &[
            (
                "a",
                "import { fromB, late } from 'b';
                 export function fromA() { return 'A'; }
                 globalThis.order = (globalThis.order || '') + 'a';
                 globalThis.seen = fromB();",
            ),
            (
                "b",
                "import { fromA } from 'a';
                 export function fromB() { return 'B' + fromA(); }
                 globalThis.order = (globalThis.order || '') + 'b';
                 globalThis.early = fromA();
                 export let late = 1;",
            ),
        ],
        "a",
    );
    assert_eq!(state(&mut r).0, PromiseState::Fulfilled);
    assert_eq!(global(&mut r.agent, "order"), "ba");
    assert_eq!(global(&mut r.agent, "early"), "A");
    assert_eq!(global(&mut r.agent, "seen"), "BA");
}

#[test]
fn reading_an_uninitialized_import_throws() {
    let mut r = run(
        &[
            ("a", "import 'b'; export let x = 1;"),
            ("b", "import { x } from 'a'; globalThis.v = x;"),
        ],
        "a",
    );
    assert_eq!(
        state(&mut r),
        (PromiseState::Rejected, "ReferenceError".to_string())
    );
}

#[test]
fn ambiguous_star_exports() {
    let files = [
        ("s1", "export var x = 1; export var only1 = 1;"),
        ("s2", "export var x = 2;"),
        ("stars", "export * from 's1'; export * from 's2';"),
        (
            "ok",
            "import * as ns from 'stars';
             globalThis.has = ('x' in ns) + ',' + ns.only1;",
        ),
        ("bad", "import { x } from 'stars';"),
        // The same binding reached twice is not ambiguous.
        ("same", "export * from 's1'; export * from 's1b';"),
        ("s1b", "export { x } from 's1';"),
        ("useSame", "import { x } from 'same'; globalThis.same = x;"),
    ];
    let mut r = run(&files, "ok");
    assert_eq!(state(&mut r).0, PromiseState::Fulfilled);
    assert_eq!(global(&mut r.agent, "has"), "false,1");
    let mut r = run(&files, "bad");
    assert_eq!(
        state(&mut r),
        (PromiseState::Rejected, "SyntaxError".to_string())
    );
    let mut r = run(&files, "useSame");
    assert_eq!(state(&mut r).0, PromiseState::Fulfilled);
    assert_eq!(global(&mut r.agent, "same"), "1");
}

#[test]
fn missing_export_is_a_link_error() {
    let mut r = run(
        &[
            ("main", "import { nope } from 'dep';"),
            ("dep", "export var yes = 1;"),
        ],
        "main",
    );
    assert_eq!(
        state(&mut r),
        (PromiseState::Rejected, "SyntaxError".to_string())
    );
}

#[test]
fn top_level_await_orders_evaluation() {
    let mut r = run(
        &[
            (
                "main",
                "import 'slow'; import 'fast';
                 globalThis.log += 'main;';",
            ),
            (
                "slow",
                "globalThis.log = 'slow1;';
                 await new Promise(function (resolve) { resolve(); });
                 globalThis.log += 'slow2;';",
            ),
            ("fast", "globalThis.log += 'fast;';"),
        ],
        "main",
    );
    assert_eq!(state(&mut r).0, PromiseState::Fulfilled);
    // `fast` does not depend on `slow`, so it runs while `slow` waits;
    // `main` runs after both.
    assert_eq!(global(&mut r.agent, "log"), "slow1;fast;slow2;main;");
}

#[test]
fn top_level_await_rejection_propagates() {
    let mut r = run(
        &[
            ("main", "import 'dep'; globalThis.ran = true;"),
            ("dep", "await 0; throw new RangeError('no');"),
        ],
        "main",
    );
    assert_eq!(
        state(&mut r),
        (PromiseState::Rejected, "RangeError".to_string())
    );
    let g = r.agent.global_object();
    let key = r.agent.heap.key_from_str("ran");
    assert!(ops::get(&mut r.agent, g, key).unwrap().is_undefined());
}

#[test]
fn errors_are_cached_per_module() {
    let mut r = run(
        &[
            ("main", "import 'boom';"),
            (
                "boom",
                "globalThis.n = (globalThis.n || 0) + 1; throw new TypeError('x');",
            ),
        ],
        "main",
    );
    assert_eq!(
        state(&mut r),
        (PromiseState::Rejected, "TypeError".to_string())
    );
    // A second import() of the failed module rejects without running it.
    evaluate_script(
        &mut r.agent,
        "import('boom').then(null, function (e) { globalThis.again = e.constructor.name; });",
    )
    .unwrap();
    drain(&mut r.agent);
    assert_eq!(global(&mut r.agent, "again"), "TypeError");
    assert_eq!(global(&mut r.agent, "n"), "1");
}

#[test]
fn dynamic_import_from_script_and_module() {
    let (mut agent, host, _realm) = setup(&[
        ("dep", "export default 7; export var named = 'n';"),
        ("dyn", "export var viaModule = 'm';"),
        (
            "outer",
            "globalThis.p = import('dyn').then(function (ns) { globalThis.fromModule = ns.viaModule; });",
        ),
    ]);
    evaluate_script(
        &mut agent,
        "import('dep').then(function (ns) { globalThis.got = ns.default + ns.named; });
         import('missing').then(null, function (e) { globalThis.missing = e.constructor.name; });
         import('dep', 1).then(null, function (e) { globalThis.badOptions = e.constructor.name; });
         import('dep').then(function (ns) { globalThis.again = ns.named; });
         import('outer');",
    )
    .unwrap();
    drain(&mut agent);
    assert_eq!(global(&mut agent, "got"), "7n");
    assert_eq!(global(&mut agent, "missing"), "TypeError");
    assert_eq!(global(&mut agent, "badOptions"), "TypeError");
    assert_eq!(global(&mut agent, "fromModule"), "m");
    assert_eq!(global(&mut agent, "again"), "n");
    // `dep` was loaded once although two import() calls asked for it.
    assert_eq!(
        host.loads.borrow().iter().filter(|s| *s == "dep").count(),
        1
    );
}

#[test]
fn json_modules() {
    let mut r = run(
        &[
            (
                "main",
                "import data from 'data.json' with { type: 'json' };
                 import * as ns from 'data.json' with { type: 'json' };
                 globalThis.out = data.a + ',' + data.list[1] + ',' + (ns.default === data);",
            ),
            ("data.json", "{ \"a\": \"x\", \"list\": [1, 2] }"),
        ],
        "main",
    );
    assert_eq!(state(&mut r).0, PromiseState::Fulfilled);
    assert_eq!(global(&mut r.agent, "out"), "x,2,true");

    let mut r = run(
        &[
            ("main", "import data from 'bad.json' with { type: 'json' };"),
            ("bad.json", "{ a: 1 }"),
        ],
        "main",
    );
    assert_eq!(
        state(&mut r),
        (PromiseState::Rejected, "SyntaxError".to_string())
    );

    let mut r = run(
        &[
            ("main", "import x from 'dep' with { other: 'y' };"),
            ("dep", "export default 1;"),
        ],
        "main",
    );
    assert_eq!(
        state(&mut r),
        (PromiseState::Rejected, "SyntaxError".to_string())
    );
}

#[test]
fn import_meta_and_this() {
    let mut r = run(
        &[(
            "main",
            "globalThis.meta = import.meta.url + ',' + (import.meta === import.meta)
                 + ',' + (this === undefined) + ',' + ((() => this)() === undefined);",
        )],
        "main",
    );
    assert_eq!(state(&mut r).0, PromiseState::Fulfilled);
    assert_eq!(global(&mut r.agent, "meta"), "mem:test,true,true,true");
}

#[test]
fn namespace_objects() {
    let mut r = run(
        &[
            (
                "main",
                "import * as ns from 'dep';
                 var out = [];
                 for (var k in ns) out[out.length] = k;
                 globalThis.keys = out.join ? out.join() : out[0] + ',' + out[1];
                 globalThis.frozen = Object.isExtensible ? Object.isExtensible(ns) : false;
                 try { ns.b = 1; } catch (e) { globalThis.setErr = e.constructor.name; }
                 globalThis.tdz = 'ok';
                 try { ns.later; } catch (e) { globalThis.tdz = e.constructor.name; }",
            ),
            ("dep", "export var b = 1, a = 2; export let later = 3;"),
        ],
        "main",
    );
    assert_eq!(state(&mut r).0, PromiseState::Fulfilled);
    // Exports are sorted by code units.
    assert!(global(&mut r.agent, "keys").starts_with("a,b"));
    assert_eq!(global(&mut r.agent, "setErr"), "TypeError");
    assert_eq!(global(&mut r.agent, "tdz"), "ok");
    assert_eq!(global(&mut r.agent, "frozen"), "false");
}

#[test]
fn script_records_do_not_keep_a_removed_realm_alive() {
    // A browser removes the realm of a closed window; the record of a
    // script that ran there must not hold it (and its objects) forever.
    let (mut agent, _host, _realm) = setup(&[]);
    let gone = agent.initialize_host_defined_realm();
    let record = module::create_script_record(&mut agent, gone, Some(Rc::from("about:blank")));
    vjs::evaluate_script_record(&mut agent, gone, "var kept = [1, 2, 3];", Some(record)).unwrap();
    agent.remove_realm(gone);
    let before = agent.heap.stats().realms;
    let report = agent
        .collect_garbage(&[])
        .expect("no native call is running");
    assert_eq!(report.freed_realms, 1);
    assert_eq!(agent.heap.stats().realms, before - 1);
}
