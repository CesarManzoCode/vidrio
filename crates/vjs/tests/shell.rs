//! Tests of the `vjs` binary: it runs on temporary files and the tests look
//! at its stdout, stderr and exit code (`print`, the uncaught error line
//! that bench/test262.py reads, `--strict`, `--module`, `$262.createRealm`,
//! `$262.evalScript` and an agent that reports).

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fresh temporary directory for one test.
fn temp_dir() -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("vjs-shell-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Writes `files` into a new directory and runs `vjs FLAGS… MAIN`.
fn vjs(flags: &[&str], files: &[(&str, &str)], main: &str) -> Output {
    let dir = temp_dir();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vjs"))
        .args(flags)
        .arg(dir.join(main))
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Output {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn script(source: &str) -> Output {
    vjs(&["--test262"], &[("t.js", source)], "t.js")
}

#[test]
fn print_writes_its_arguments() {
    let out = script("print('hello', 1 + 1); print(true);");
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "hello 2\ntrue\n");
    assert_eq!(out.stderr, "");
}

#[test]
fn uncaught_errors_use_the_name_property() {
    let out = script("print('before'); throw new TypeError('bad thing');");
    assert_eq!(out.code, 1);
    assert_eq!(out.stdout, "before\n");
    assert_eq!(out.stderr, "Uncaught TypeError: bad thing\n");

    let out = script("var e = new Error('m'); e.name = 'Custom'; throw e;");
    assert_eq!(out.stderr, "Uncaught Custom: m\n");

    let out = script("throw 42;");
    assert_eq!(out.code, 1);
    assert_eq!(out.stderr, "Uncaught 42\n");
}

#[test]
fn parse_errors_are_uncaught_syntax_errors() {
    let out = script("print('never');\nvar = ;");
    assert_eq!(out.code, 1);
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr.starts_with("Uncaught SyntaxError: "),
        "{}",
        out.stderr
    );
    assert_eq!(out.stderr.lines().count(), 1);

    let out = vjs(&["--module"], &[("m.mjs", "export export;")], "m.mjs");
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.starts_with("Uncaught SyntaxError: "),
        "{}",
        out.stderr
    );
}

#[test]
fn strict_flag_makes_scripts_strict() {
    let out = vjs(&["--strict"], &[("t.js", "undeclared = 1;")], "t.js");
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.starts_with("Uncaught ReferenceError"),
        "{}",
        out.stderr
    );
    let out = vjs(
        &[],
        &[("t.js", "undeclared = 1; print(undeclared);")],
        "t.js",
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "1\n");
}

#[test]
fn jobs_are_drained_after_the_script() {
    let out = script(
        "new Promise(function (r) { r(1); }).then(function (v) { print('job ' + v); });
         print('sync');",
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "sync\njob 1\n");
}

#[test]
fn modules_resolve_from_the_file_system() {
    let out = vjs(
        &["--module", "--test262"],
        &[
            (
                "main.mjs",
                "import { value } from './dep.mjs';
                 import * as self from './main.mjs';
                 import data from './data.json' with { type: 'json' };
                 export var mine = 'mine';
                 print(value, self.mine, data.k);
                 await 0;
                 import('./dep.mjs').then(function (ns) { print('dynamic', ns.value); });",
            ),
            ("dep.mjs", "export const value = 'dep';"),
            ("data.json", "{\"k\": \"json\"}"),
        ],
        "main.mjs",
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "dep mine json\ndynamic dep\n");
}

#[test]
fn module_errors_are_uncaught() {
    let out = vjs(
        &["--module"],
        &[
            ("m.mjs", "import { nothing } from './d.mjs';"),
            ("d.mjs", "export var x;"),
        ],
        "m.mjs",
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.starts_with("Uncaught SyntaxError: "),
        "{}",
        out.stderr
    );

    let out = vjs(
        &["--module"],
        &[("m.mjs", "await 0; throw new RangeError('late');")],
        "m.mjs",
    );
    assert_eq!(out.code, 1);
    assert_eq!(out.stderr, "Uncaught RangeError: late\n");
}

#[test]
fn dynamic_import_from_a_script_is_relative_to_its_file() {
    let out = vjs(
        &[],
        &[
            (
                "t.js",
                "import('./f.mjs').then(function (ns) { print(ns.default); });",
            ),
            ("f.mjs", "export default 'fixture';"),
        ],
        "t.js",
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "fixture\n");
}

#[test]
fn create_realm_gives_a_new_global_with_its_own_262() {
    let out = script(
        "var other = $262.createRealm();
         print(other.global === globalThis, typeof other.evalScript);
         print(other.evalScript('var x = 20; x + 1'));
         print(other.global.x, typeof x);
         var e = other.evalScript('new TypeError(\"t\")');
         print(e instanceof TypeError, e instanceof other.global.TypeError);
         var third = other.createRealm();
         print(third.global === other.global);",
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        "false function\n21\n20 undefined\nfalse true\nfalse\n"
    );
}

#[test]
fn eval_script_runs_global_code_and_throws_syntax_errors() {
    let out = script(
        "$262.evalScript('var y = 5; let z = 6;');
         print(y, z, $262.global === globalThis);
         try { $262.evalScript('var = ;'); } catch (e) { print(e.constructor === SyntaxError); }
         print(typeof $262.IsHTMLDDA, $262.IsHTMLDDA() === null);
         $262.gc();",
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "5 6 true\ntrue\nundefined true\n");
}

#[test]
fn an_agent_reports_to_the_main_thread() {
    let out = script(
        "$262.agent.start('$262.agent.report(\"hello from \" + typeof $262.agent.receiveBroadcast); $262.agent.leaving();');
         var r;
         var start = $262.agent.monotonicNow();
         while ((r = $262.agent.getReport()) === null) {
           if ($262.agent.monotonicNow() - start > 5000) throw new Error('no report');
           $262.agent.sleep(5);
         }
         print(r);
         print($262.agent.getReport());",
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "hello from function\nnull\n");
}
