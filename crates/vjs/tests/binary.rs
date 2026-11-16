//! Binary data through scripts: TypedArray and DataView views over
//! resizable, growable and detached buffers, the float16 conversions at
//! their edges (Float16Array and DataView), the base64 and hex methods of
//! Uint8Array with both alphabets and every last-chunk option, and
//! `Atomics.wait`/`notify`/`waitAsync` between two agents of the `vjs`
//! binary.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use vjs::agent::Agent;
use vjs::ops;
use vjs::value::{PropertyKey, Value};
use vjs::waiter::AsyncWait;
use vjs::{Throw, evaluate_script};

const PRELUDE: &str = r#"
function fail(msg) { throw new Error(msg); }
function str(v) { return typeof v === "symbol" ? v.toString() : "" + v; }
function same(actual, expected, what) {
  if (!Object.is(actual, expected)) {
    fail(what + ": expected " + str(expected) + " (" + typeof expected + "), got " +
         str(actual) + " (" + typeof actual + ")");
  }
}
function list(actual, expected, what) {
  same(actual.length, expected.length, what + " length");
  for (var i = 0; i < expected.length; i++) same(actual[i], expected[i], what + " [" + i + "]");
}
function throws(C, f, what) {
  try { f(); } catch (e) {
    if (!(e instanceof C)) fail(what + ": wrong error " + e);
    return;
  }
  fail(what + ": did not throw");
}
"#;

fn show(agent: &mut Agent, v: Value) -> String {
    match ops::to_string(agent, v) {
        Ok(s) => agent.heap.string_to_rust(s),
        Err(_) => "<unprintable>".to_string(),
    }
}

fn show_throw(agent: &mut Agent, Throw(v): Throw) -> String {
    if let Value::Object(o) = v {
        let name: PropertyKey = agent.heap.key_from_str("name");
        let message: PropertyKey = agent.heap.key_from_str("message");
        if let (Ok(n), Ok(m)) = (ops::get(agent, o, name), ops::get(agent, o, message)) {
            return format!("throw {}: {}", show(agent, n), show(agent, m));
        }
    }
    format!("throw {}", show(agent, v))
}

fn eval(agent: &mut Agent, src: &str) -> String {
    match evaluate_script(agent, src) {
        Ok(v) => show(agent, v),
        Err(e) => show_throw(agent, e),
    }
}

fn ok(src: &str) {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let source = format!("{PRELUDE}\n{src}");
    assert_eq!(eval(&mut agent, &source), "ok", "script:\n{src}");
}

// ----- views over resizable and detached buffers -----

#[test]
fn views_follow_a_resized_buffer() {
    ok(r#"
      var rab = new ArrayBuffer(8, { maxByteLength: 16 });
      var fixed = new Uint8Array(rab, 0, 4);
      var tracking = new Uint8Array(rab, 2);
      var dv = new DataView(rab, 1);
      same(rab.resizable, true, "resizable");
      same(tracking.length, 6, "tracking length");
      fixed.set([1, 2, 3, 4]);
      list(tracking.subarray(0, 2), [3, 4], "shared bytes");

      rab.resize(12);
      same(tracking.length, 10, "tracking grows");
      same(tracking.byteLength, 10, "tracking byteLength");
      same(fixed.length, 4, "fixed keeps its length");
      same(dv.byteLength, 11, "tracking DataView grows");
      same(tracking[9], 0, "new bytes are zero");

      rab.resize(3);
      same(fixed.length, 0, "fixed view out of bounds");
      same(fixed.byteOffset, 0, "out of bounds byteOffset");
      same(fixed[0], undefined, "out of bounds read");
      throws(TypeError, function () { fixed.fill(0); }, "methods validate the view");
      same(tracking.length, 1, "tracking shrinks");
      list(Array.from(tracking), [3], "tracking contents");

      rab.resize(1);
      same(tracking.length, 0, "offset past the end");
      throws(TypeError, function () { tracking.join(); }, "tracking out of bounds");
      same(dv.byteLength, 0, "a DataView at the end has no bytes");
      throws(RangeError, function () { dv.getUint8(0); }, "DataView index past its end");
      rab.resize(0);
      throws(TypeError, function () { dv.byteLength; }, "DataView out of bounds");

      rab.resize(8);
      same(fixed.length, 4, "fixed view back in bounds");
      list(fixed, [0, 0, 0, 0], "bytes past a shrink come back zeroed");
      throws(RangeError, function () { rab.resize(17); }, "past maxByteLength");

      // A length-tracking subarray tracks too; one with an end does not.
      var sub = tracking.subarray(1);
      var sub2 = tracking.subarray(1, 3);
      rab.resize(16);
      same(sub.length, 13, "tracking subarray");
      same(sub2.length, 2, "fixed subarray");

      // Shrinking inside a callback: reads past the end are undefined.
      var seen = [];
      var u = new Uint8Array(rab);
      u.forEach(function (v, i) { if (i === 0) rab.resize(2); seen.push(v); });
      same(seen.length, 16, "forEach visits the original length");
      same(seen[2], undefined, "element past the new end");

      // A growable SharedArrayBuffer only grows.
      var g = new SharedArrayBuffer(4, { maxByteLength: 8 });
      var gv = new Int32Array(g);
      same(gv.length, 1, "growable view");
      g.grow(8);
      same(gv.length, 2, "growable view after grow");
      throws(RangeError, function () { g.grow(4); }, "shrinking a shared buffer");
      "ok";
    "#);
}

#[test]
fn views_of_a_detached_buffer() {
    ok(r#"
      var ab = new ArrayBuffer(8);
      var u = new Uint16Array(ab);
      u[0] = 0x1234;
      var t = ab.transfer(16);
      same(ab.detached, true, "detached");
      same(ab.byteLength, 0, "detached byteLength");
      same(u.length, 0, "view length");
      same(u.byteLength, 0, "view byteLength");
      same(u[0], undefined, "read");
      u[0] = 5;
      same(Object.keys(u).length, 0, "no indices");
      same(0 in u, false, "has");
      throws(TypeError, function () { u.at(0); }, "methods throw");
      throws(TypeError, function () { new Uint8Array(ab); }, "new view");
      throws(TypeError, function () { new DataView(ab); }, "new DataView");
      throws(TypeError, function () { ab.slice(0); }, "slice");
      same(new Uint16Array(t)[0], 0x1234, "transferred contents");
      same(t.byteLength, 16, "transfer grows");
      same(t.resizable, false, "fixed stays fixed");

      var r = new ArrayBuffer(4, { maxByteLength: 8 });
      same(r.transfer().resizable, true, "transfer keeps resizability");
      var r2 = new ArrayBuffer(4, { maxByteLength: 8 });
      var f = r2.transferToFixedLength(2);
      same(f.resizable, false, "transferToFixedLength");
      same(f.byteLength, 2, "transferToFixedLength length");

      // Detaching while iterating or sorting.
      var b = new ArrayBuffer(4);
      var v = new Uint8Array(b);
      var it = v.values();
      it.next();
      b.transfer();
      throws(TypeError, function () { it.next(); }, "iterator over a detached view");
      var c = new ArrayBuffer(4);
      var w = new Uint8Array(c);
      w.set([3, 1, 2, 0]);
      w.sort(function (x, y) { if (!c.detached) c.transfer(); return x - y; });
      same(w.length, 0, "sorted view of a detached buffer");
      "ok";
    "#);
}

#[test]
fn constructors_conversions_and_species() {
    ok(r#"
      same(Float16Array.BYTES_PER_ELEMENT, 2, "BYTES_PER_ELEMENT");
      same(Object.getPrototypeOf(Int8Array), Object.getPrototypeOf(Float16Array), "%TypedArray%");
      throws(TypeError, function () { Object.getPrototypeOf(Int8Array)(); }, "abstract");
      throws(TypeError, function () { Int8Array(1); }, "call without new");
      list(new Uint8ClampedArray([-1, 1.5, 2.5, 300]), [0, 2, 2, 255], "clamped");
      list(new Int8Array([255, 128]), [-1, -128], "wrapping");
      var big = new BigInt64Array([2n ** 63n]);
      same(big[0], -(2n ** 63n), "BigInt64 wraps");
      throws(TypeError, function () { new BigInt64Array([1]); }, "Number into BigInt64");
      throws(TypeError, function () { new Int8Array(big); }, "content type mismatch");
      throws(RangeError, function () { new Int32Array(new ArrayBuffer(6), 2); }, "misaligned");
      throws(RangeError, function () { new Int32Array(new ArrayBuffer(6)); }, "length not a multiple");
      list(new Int16Array(new Float64Array([1.9, -1.9])), [1, -1], "from another kind");
      var a = Uint8Array.from({ length: 3, 0: 7 }, function (x) { return (x | 0) + 1; });
      list(a, [8, 1, 1], "from array-like with map");
      list(Int8Array.of(1, 2, 3).reverse(), [3, 2, 1], "of and reverse");
      list(new Float32Array([3, NaN, -0, 0, -1]).sort(), [-1, -0, 0, 3, NaN], "numeric sort");
      var s = new Uint8Array([1, 2, 3, 4]);
      s.set([9, 9], 2);
      list(s, [1, 2, 9, 9], "set from array-like");
      s.set(s.subarray(0, 2), 1);
      list(s, [1, 1, 2, 9], "set from an overlapping view");
      s.copyWithin(0, 1);
      list(s, [1, 2, 9, 9], "copyWithin");
      throws(RangeError, function () { s.set([1], 4); }, "set past the end");
      same(s.toString(), "1,2,9,9", "toString");
      same(Uint8Array.prototype.toString, Array.prototype.toString, "shared toString");
      same(s[Symbol.iterator], s.values, "@@iterator");
      same(Object.prototype.toString.call(s), "[object Uint8Array]", "toStringTag");
      list(s.with(-1, 0), [1, 2, 9, 0], "with");
      list(s.toReversed(), [9, 9, 2, 1], "toReversed");
      class Sub extends Uint8Array {}
      var m = new Sub([1, 2]).map(function (x) { return x * 2; });
      same(m instanceof Sub, true, "species");
      list(m, [2, 4], "map");
      same(ArrayBuffer.isView(new DataView(new ArrayBuffer(1))), true, "isView");
      "ok";
    "#);
}

// ----- float16 -----

#[test]
fn float16_conversions_at_the_edges() {
    ok(r#"
      var p = Math.pow;
      var f = new Float16Array(1);
      function f16(x) { f[0] = x; return f[0]; }
      same(f16(65504), 65504, "largest finite");
      same(f16(65519.99), 65504, "just below the overflow tie");
      same(f16(65520), Infinity, "overflow tie rounds to infinity");
      same(f16(-65520), -Infinity, "negative overflow");
      same(f16(p(2, -24)), p(2, -24), "smallest subnormal");
      same(f16(p(2, -25)), 0, "half the smallest subnormal ties to zero");
      same(f16(-p(2, -25)), -0, "negative tie keeps the sign");
      same(f16(p(2, -25) * 1.5), p(2, -24), "above the tie rounds up");
      same(f16(1023 * p(2, -24)), 1023 * p(2, -24), "largest subnormal");
      same(f16(p(2, -14)), p(2, -14), "smallest normal");
      same(f16(1 + p(2, -11)), 1, "tie to even (down)");
      same(f16(1 + 3 * p(2, -11)), 1 + p(2, -9), "tie to even (up)");
      same(f16(1 + p(2, -11) + p(2, -40)), 1 + p(2, -10), "no double rounding");
      same(f16(-0), -0, "negative zero");
      same(f16(NaN), NaN, "NaN");
      same(f16(Infinity), Infinity, "infinity");

      var dv = new DataView(new ArrayBuffer(4));
      dv.setFloat16(0, 1.5);
      list(new Uint8Array(dv.buffer, 0, 2), [0x3e, 0x00], "big-endian bytes");
      dv.setFloat16(2, 1.5, true);
      list(new Uint8Array(dv.buffer, 2, 2), [0x00, 0x3e], "little-endian bytes");
      same(dv.getFloat16(0), 1.5, "getFloat16");
      same(dv.getFloat16(2, true), 1.5, "getFloat16 little-endian");
      dv.setUint16(0, 0x7bff);
      same(dv.getFloat16(0), 65504, "bits of the largest finite");
      dv.setUint16(0, 0x0001);
      same(dv.getFloat16(0), p(2, -24), "bits of the smallest subnormal");
      dv.setUint16(0, 0xfc00);
      same(dv.getFloat16(0), -Infinity, "bits of -Infinity");
      dv.setFloat16(0, 65520);
      same(dv.getUint16(0), 0x7c00, "DataView rounds like Float16Array");
      throws(RangeError, function () { dv.getFloat16(3); }, "past the end");
      "ok";
    "#);
}

// ----- base64 and hex -----

#[test]
fn base64_alphabets_and_last_chunk_handling() {
    ok(r#"
      var bytes = new Uint8Array([251, 255]);
      same(bytes.toBase64(), "+/8=", "standard");
      same(bytes.toBase64({ alphabet: "base64url" }), "-_8=", "url");
      same(bytes.toBase64({ alphabet: "base64url", omitPadding: true }), "-_8", "omitPadding");
      list(Uint8Array.fromBase64("-_8", { alphabet: "base64url" }), [251, 255], "url decode");
      throws(SyntaxError, function () { Uint8Array.fromBase64("-_8="); }, "url chars in standard");
      throws(SyntaxError, function () {
        Uint8Array.fromBase64("+/8=", { alphabet: "base64url" });
      }, "standard chars in url");
      throws(TypeError, function () { Uint8Array.fromBase64("AA", { alphabet: "other" }); },
             "unknown alphabet");
      throws(TypeError, function () { Uint8Array.fromBase64(1); }, "not a string");
      list(Uint8Array.fromBase64(" Zm9v\nYmFy "), [102, 111, 111, 98, 97, 114], "whitespace");

      list(Uint8Array.fromBase64("Zm9vYg"), [102, 111, 111, 98], "loose partial chunk");
      throws(SyntaxError, function () {
        Uint8Array.fromBase64("Zm9vYg", { lastChunkHandling: "strict" });
      }, "strict needs padding");
      list(Uint8Array.fromBase64("Zm9vYg", { lastChunkHandling: "stop-before-partial" }),
           [102, 111, 111], "stop-before-partial");
      list(Uint8Array.fromBase64("Zm9vYg==", { lastChunkHandling: "strict" }),
           [102, 111, 111, 98], "strict with padding");
      throws(SyntaxError, function () {
        Uint8Array.fromBase64("Zm9vYh==", { lastChunkHandling: "strict" });
      }, "strict rejects non-zero padding bits");
      list(Uint8Array.fromBase64("Zm9vYh=="), [102, 111, 111, 98], "loose ignores them");
      throws(SyntaxError, function () { Uint8Array.fromBase64("Zm9vY"); }, "lone character");
      throws(TypeError, function () {
        Uint8Array.fromBase64("", { lastChunkHandling: "lax" });
      }, "unknown lastChunkHandling");

      var into = new Uint8Array(4);
      var r = into.setFromBase64("Zm9vYmFy");
      same(r.read, 4, "read stops at a whole chunk");
      same(r.written, 3, "written");
      list(into, [102, 111, 111, 0], "partial fill");
      var into2 = new Uint8Array(8);
      r = into2.setFromBase64("Zm9vYg", { lastChunkHandling: "stop-before-partial" });
      same(r.read, 4, "stop-before-partial read");
      same(r.written, 3, "stop-before-partial written");
      var into3 = new Uint8Array(8);
      throws(SyntaxError, function () { into3.setFromBase64("Zm9v$"); }, "bad character");
      list(into3.subarray(0, 4), [102, 111, 111, 0], "bytes before the error are written");

      same(new Uint8Array([0, 171, 16]).toHex(), "00ab10", "toHex");
      list(Uint8Array.fromHex("00AB10"), [0, 171, 16], "fromHex");
      throws(SyntaxError, function () { Uint8Array.fromHex("abc"); }, "odd length");
      var h = new Uint8Array(2);
      r = h.setFromHex("aabbcc");
      same(r.read, 4, "setFromHex read");
      same(r.written, 2, "setFromHex written");
      throws(TypeError, function () { Uint8Array.prototype.toBase64.call(new Int8Array(1)); },
             "only Uint8Array");
      "ok";
    "#);
}

// ----- Atomics -----

#[test]
fn atomics_on_one_agent() {
    ok(r#"
      var ia = new Int32Array(new SharedArrayBuffer(16));
      same(Atomics.add(ia, 0, 5), 0, "add returns the old value");
      same(Atomics.sub(ia, 0, 2), 5, "sub");
      same(Atomics.compareExchange(ia, 0, 3, 9), 3, "compareExchange");
      same(Atomics.load(ia, 0), 9, "load");
      same(Atomics.exchange(ia, 0, -1), 9, "exchange");
      same(Atomics.and(ia, 0, 0xff), -1, "and");
      same(Atomics.or(ia, 0, 0x100), 0xff, "or");
      same(Atomics.xor(ia, 0, 0x1ff), 0x1ff, "xor");
      same(Atomics.store(ia, 1, -0), 0, "store returns the integer");
      var u8 = new Uint8Array(new ArrayBuffer(2));
      same(Atomics.add(u8, 1, 300), 0, "non-shared add");
      same(u8[1], 44, "wraps to the element type");
      var b = new BigInt64Array(new SharedArrayBuffer(16));
      same(Atomics.sub(b, 1, 1n), 0n, "BigInt sub");
      same(b[1], -1n, "BigInt wraps");
      throws(TypeError, function () { Atomics.add(new Float64Array(1), 0, 1); }, "float array");
      throws(TypeError, function () { Atomics.add(new Uint8ClampedArray(1), 0, 1); }, "clamped");
      throws(RangeError, function () { Atomics.load(ia, 4); }, "index out of range");
      throws(TypeError, function () { Atomics.wait(new Int32Array(4), 0, 0); }, "non-shared wait");
      throws(TypeError, function () { Atomics.wait(new Int16Array(new SharedArrayBuffer(4)), 0, 0); },
             "wait on Int16Array");
      same(Atomics.notify(new Int32Array(4), 0), 0, "notify on a non-shared array");
      same(Atomics.wait(ia, 2, 1, 0), "not-equal", "not-equal");
      same(Atomics.wait(ia, 2, 0, 10), "timed-out", "timed-out");
      same(Atomics.isLockFree(4), true, "isLockFree(4)");
      same(Atomics.pause(), undefined, "pause");
      throws(TypeError, function () { Atomics.pause(1.5); }, "pause with a fraction");
      var w = Atomics.waitAsync(ia, 2, 1);
      same(w.async, false, "waitAsync not-equal is synchronous");
      same(w.value, "not-equal", "waitAsync not-equal");
      w = Atomics.waitAsync(ia, 2, 0, 0);
      same(w.value, "timed-out", "waitAsync zero timeout");
      w = Atomics.waitAsync(ia, 3, 0);
      same(w.async, true, "waitAsync waits");
      var settled;
      w.value.then(function (v) { settled = v; });
      same(Atomics.notify(ia, 3), 1, "notify counts the async waiter");
      "ok";
    "#);
}

#[test]
fn wait_async_times_out_in_the_job_loop() {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let setup = r#"
      var result = "pending";
      var ia = new Int32Array(new SharedArrayBuffer(8));
      Atomics.waitAsync(ia, 0, 0, 20).value.then(function (v) { result = v; });
      "ok";
    "#;
    assert_eq!(eval(&mut agent, setup), "ok");
    assert!(agent.has_pending_async_waits());
    assert!(agent.settle_async_waits(AsyncWait::Timeouts));
    assert!(agent.run_jobs().is_empty());
    assert!(!agent.has_pending_async_waits());
    assert_eq!(eval(&mut agent, "result"), "timed-out");
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Runs `source` with `vjs --test262` and returns its exit code, stdout
/// and stderr.
fn run_shell(source: &str) -> (i32, String, String) {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir: PathBuf = std::env::temp_dir().join(format!("vjs-binary-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.js"), source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vjs"))
        .arg("--test262")
        .arg(dir.join("t.js"))
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Helpers of the shell scripts: a report with a deadline, and notifying
/// until a waiter is woken (the agent may not be waiting yet).
const SHELL_PRELUDE: &str = r#"
function report() {
  var start = $262.agent.monotonicNow(), r;
  while ((r = $262.agent.getReport()) === null) {
    if ($262.agent.monotonicNow() - start > 10000) throw new Error("no report");
    $262.agent.sleep(2);
  }
  return r;
}
function notifyOne(ia, index) {
  var start = $262.agent.monotonicNow();
  while (Atomics.notify(ia, index, 1) === 0) {
    if ($262.agent.monotonicNow() - start > 10000) throw new Error("nobody waited");
    $262.agent.sleep(2);
  }
}
"#;

#[test]
fn wait_and_notify_between_two_agents() {
    let script = format!(
        "{SHELL_PRELUDE}\n{}",
        r#"
        $262.agent.start(`
          $262.agent.receiveBroadcast(function (sab) {
            var ia = new Int32Array(sab);
            Atomics.add(ia, 1, 1);
            var r = Atomics.wait(ia, 0, 0, 10000);
            $262.agent.report("woke " + r);
            $262.agent.report("timeout " + Atomics.wait(ia, 2, 0, 5));
            $262.agent.leaving();
          });
        `);
        var ia = new Int32Array(new SharedArrayBuffer(16));
        $262.agent.broadcast(ia.buffer);
        notifyOne(ia, 0);
        print(report());
        print(report());
        print(Atomics.load(ia, 1));
        "#
    );
    let (code, stdout, stderr) = run_shell(&script);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "woke ok\ntimeout timed-out\n1\n");
}

#[test]
fn wait_async_is_notified_by_another_agent() {
    let script = format!(
        "{SHELL_PRELUDE}\n{}",
        r#"
        $262.agent.start(`
          $262.agent.receiveBroadcast(function (sab) {
            var ia = new Int32Array(sab);
            var r = Atomics.waitAsync(ia, 0, 0);
            $262.agent.report("async " + r.async);
            r.value.then(function (v) {
              $262.agent.report("settled " + v);
              $262.agent.leaving();
            });
          });
        `);
        var ia = new Int32Array(new SharedArrayBuffer(16));
        $262.agent.broadcast(ia.buffer);
        print(report());
        notifyOne(ia, 0);
        print(report());
        // The main agent's own waiter settles by timeout after the script.
        Atomics.waitAsync(ia, 1, 0, 10).value.then(function (v) { print("main " + v); });
        "#
    );
    let (code, stdout, stderr) = run_shell(&script);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "async true\nsettled ok\nmain timed-out\n");
}
