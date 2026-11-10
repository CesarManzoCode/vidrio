import importlib.util
import tempfile
import unittest
from pathlib import Path

BENCH = Path(__file__).resolve().parent


def _load(name):
    spec = importlib.util.spec_from_file_location(name, BENCH / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


t262 = _load("test262")

INLINE = """\
// Copyright header
/*---
esid: sec-example
description: |
  Block text that looks like keys:
  flags: [raw]
  - not-an-include.js
includes: [compareArray.js, "propertyHelper.js"]
flags: [onlyStrict, async]
features: [Symbol, 'Proxy']
---*/
code();
"""

BLOCK = """\
/*---
info: >
  Folded text.
includes:
  - compareArray.js
  - deepEqual.js
features:
  - class-fields-private
  - decorators
negative:
  phase: parse
  type: SyntaxError
---*/
$DONOTEVALUATE();
"""

FEATURES_TXT = """\
## Proposed language features
#
# Comment lines are ignored.
#
# Decorators
# https://github.com/tc39/proposal-decorators
decorators

# Source Phase Imports
## https://github.com/tc39/proposal-source-phase-imports
source-phase-imports
## test262 special specifier
source-phase-imports-module-source

## Standard language features
#
Symbol
Temporal

## Test-Harness Features
IsHTMLDDA
"""

HARNESS = {
    "assert.js": "// assert.js",
    "sta.js": "// sta.js",
    "doneprintHandle.js": "// doneprintHandle.js",
    "compareArray.js": "// compareArray.js",
    "propertyHelper.js": "// propertyHelper.js",
}


def meta(**fields):
    return t262.Meta(**fields)


def outcome(exit_code=0, stdout="", stderr="", timed_out=False):
    return t262.Outcome(exit_code, stdout, stderr, timed_out)


class FrontmatterTest(unittest.TestCase):
    def test_inline_lists(self):
        parsed = t262.parse_frontmatter(INLINE)
        self.assertEqual(parsed.includes, ["compareArray.js", "propertyHelper.js"])
        self.assertEqual(parsed.flags, ["onlyStrict", "async"])
        self.assertEqual(parsed.features, ["Symbol", "Proxy"])
        self.assertIsNone(parsed.negative)

    def test_block_lists_and_negative(self):
        parsed = t262.parse_frontmatter(BLOCK)
        self.assertEqual(parsed.includes, ["compareArray.js", "deepEqual.js"])
        self.assertEqual(parsed.features, ["class-fields-private", "decorators"])
        self.assertEqual(parsed.flags, [])
        self.assertEqual(parsed.negative, {"phase": "parse", "type": "SyntaxError"})

    def test_flow_list_over_several_lines(self):
        parsed = t262.parse_frontmatter("/*---\nfeatures: [a,\n  b, c]\nflags: [raw]\n---*/")
        self.assertEqual(parsed.features, ["a", "b", "c"])
        self.assertEqual(parsed.flags, ["raw"])

    def test_without_frontmatter(self):
        parsed = t262.parse_frontmatter("var x = 1;\n")
        self.assertEqual((parsed.includes, parsed.flags, parsed.features), ([], [], []))
        self.assertIsNone(parsed.negative)


class SelectionTest(unittest.TestCase):
    def test_excluded_features_stop_at_standard_section(self):
        self.assertEqual(
            t262.excluded_features(FEATURES_TXT),
            ["decorators", "source-phase-imports", "source-phase-imports-module-source"],
        )

    def test_skip_rules(self):
        excluded = t262.excluded_features(FEATURES_TXT)
        self.assertEqual(
            t262.skip_reason(meta(features=["Symbol", "decorators"]), excluded),
            "feature decorators",
        )
        self.assertIsNone(t262.skip_reason(meta(features=["Temporal", "IsHTMLDDA"]), excluded))
        self.assertEqual(
            t262.skip_reason(meta(flags=["CanBlockIsFalse"]), excluded), "flag CanBlockIsFalse"
        )
        self.assertIsNone(t262.skip_reason(meta(flags=["CanBlockIsTrue"]), excluded))

    def test_scenarios_per_flag(self):
        cases = {
            ("raw",): ["raw"],
            ("module",): ["module"],
            ("module", "async"): ["module"],
            ("onlyStrict",): ["strict"],
            ("noStrict",): ["sloppy"],
            ("async",): ["sloppy", "strict"],
            ("generated", "CanBlockIsTrue"): ["sloppy", "strict"],
            (): ["sloppy", "strict"],
        }
        for flags, expected in cases.items():
            with self.subTest(flags=flags):
                self.assertEqual(t262.scenarios(meta(flags=list(flags))), expected)

    def test_discover_skips_fixtures_and_temporary_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel in (
                "test/language/a.js",
                "test/language/b_FIXTURE.js",
                "test/language/data.json",
                "test/language/a.vidrio-tmp-strict-1-2.js",
                "test/language/.vidrio-tmp-1-3/a.js",
                "test/built-ins/Array/c.js",
                "test/intl402/d.js",
            ):
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_text("")
            found = [rel for rel, _ in t262.discover(root, ("test/language", "test/built-ins"))]
            self.assertEqual(found, ["test/language/a.js", "test/built-ins/Array/c.js"])
            filtered = [rel for rel, _ in t262.discover(root, ("test/built-ins",), "test/built-ins/Arr")]
            self.assertEqual(filtered, ["test/built-ins/Array/c.js"])

    def test_imports_itself(self):
        self.assertTrue(t262.imports_itself("a.js", "import * as ns from './a.js';", []))
        self.assertTrue(t262.imports_itself("a.js", "", ['import "./a.js";']))
        self.assertFalse(t262.imports_itself("a.js", "import './a.json'; import '../a.js';", []))


class SourceTest(unittest.TestCase):
    def build(self, parsed, mode):
        return t262.build_source(parsed, "body();\n", mode, HARNESS.__getitem__)

    def test_prelude_order(self):
        parsed = meta(flags=["async"], includes=["compareArray.js", "propertyHelper.js"])
        self.assertEqual(
            self.build(parsed, "sloppy"),
            "// assert.js\n// sta.js\n// doneprintHandle.js\n"
            "// compareArray.js\n// propertyHelper.js\nbody();\n",
        )

    def test_strict_directive_is_first_line(self):
        code = self.build(meta(includes=["compareArray.js"]), "strict")
        self.assertTrue(code.startswith('"use strict";\n// assert.js\n// sta.js\n'))
        self.assertTrue(code.endswith("// compareArray.js\nbody();\n"))

    def test_module_gets_prelude_without_directive(self):
        code = self.build(meta(flags=["module"]), "module")
        self.assertEqual(code, "// assert.js\n// sta.js\nbody();\n")

    def test_raw_is_unmodified(self):
        self.assertEqual(self.build(meta(flags=["raw"], includes=["compareArray.js"]), "raw"), "body();\n")

    def test_commands(self):
        node = t262.command("node", "/usr/bin/node", "/t/a.js", "module")
        self.assertEqual(
            node,
            ["/usr/bin/node", "--experimental-vm-modules", "--expose-gc", str(t262.NODE_HOST), "--module", "/t/a.js"],
        )
        self.assertEqual(
            t262.command("boa", "boa", "/t/a.js", "strict"), ["boa", "--test262-object", "--strict", "/t/a.js"]
        )
        self.assertEqual(t262.command("boa", "boa", "/t/a.js", "module"), ["boa", "--test262-object", "-m", "/t/a.js"])
        self.assertEqual(t262.command("vjs", "vjs", "/t/a.js", "sloppy"), ["vjs", "--test262", "/t/a.js"])
        self.assertEqual(
            t262.command("vjs", "vjs", "/t/a.js", "module"), ["vjs", "--test262", "--module", "/t/a.js"]
        )


class DecisionTest(unittest.TestCase):
    NEG_PARSE = meta(negative={"phase": "parse", "type": "SyntaxError"})
    NEG_RUNTIME = meta(negative={"phase": "runtime", "type": "ReferenceError"})
    ASYNC = meta(flags=["async"])

    def result(self, parsed, out, engine="node"):
        return t262.decide(parsed, out, engine, 10)[0]

    def test_plain_pass_and_fail(self):
        for engine in ("node", "vjs"):
            with self.subTest(engine=engine):
                self.assertEqual(self.result(meta(), outcome(), engine), "pass")
                failed = outcome(1, stderr="Uncaught Test262Error: Expected true\n")
                self.assertEqual(self.result(meta(), failed, engine), "fail")
                self.assertEqual(self.result(meta(), outcome(139), engine), "fail")

    def test_negative_matches_error_name(self):
        out = outcome(1, stderr="Uncaught SyntaxError: Identifier 'x' has already been declared\n")
        self.assertEqual(self.result(self.NEG_PARSE, out), "pass")
        out = outcome(1, stderr="Uncaught ReferenceError: x is not defined\n")
        self.assertEqual(self.result(self.NEG_RUNTIME, out, "vjs"), "pass")
        self.assertEqual(self.result(self.NEG_PARSE, out), "fail")

    def test_negative_without_error_fails(self):
        result, detail = t262.decide(self.NEG_RUNTIME, outcome(), "node", 10)
        self.assertEqual(result, "fail")
        self.assertIn("completed without error", detail)

    def test_negative_parse_reaching_donotevaluate_fails(self):
        out = outcome(1, stderr="Uncaught string: Test262: This statement should not be evaluated.\n")
        self.assertEqual(self.result(self.NEG_PARSE, out), "fail")
        boa = outcome(1, stderr="\x1b[31mUncaught\x1b[0m: \"Test262: This statement should not be evaluated.\"\n")
        self.assertEqual(self.result(self.NEG_PARSE, boa, "boa"), "fail")

    def test_uncaught_line_needs_exit_one(self):
        out = outcome(0, stderr="Uncaught SyntaxError: x\n")
        self.assertEqual(self.result(self.NEG_PARSE, out), "fail")
        self.assertEqual(self.result(meta(), out), "fail")

    def test_async(self):
        complete = outcome(0, stdout="Test262:AsyncTestComplete\n")
        self.assertEqual(self.result(self.ASYNC, complete), "pass")
        failure = outcome(0, stdout="Test262:AsyncTestFailure:Test262Error: boom\n")
        self.assertEqual(self.result(self.ASYNC, failure), "fail")
        both = outcome(0, stdout="Test262:AsyncTestFailure:Test262Error: boom\nTest262:AsyncTestComplete\n")
        self.assertEqual(self.result(self.ASYNC, both), "fail")
        self.assertEqual(self.result(self.ASYNC, outcome(0)), "fail")
        crashed = outcome(1, stdout="Test262:AsyncTestComplete\n", stderr="Uncaught TypeError: x\n")
        self.assertEqual(self.result(self.ASYNC, crashed), "fail")
        self.assertEqual(self.result(self.ASYNC, complete, "boa"), "pass")

    def test_timeout_fails(self):
        late = outcome(None, stdout="Test262:AsyncTestComplete\n", timed_out=True)
        for parsed in (meta(), self.ASYNC, self.NEG_RUNTIME):
            result, detail = t262.decide(parsed, late, "node", 10)
            self.assertEqual(result, "fail")
            self.assertTrue(detail.startswith("timeout after 10 s"))

    def test_detail_keeps_last_lines(self):
        stderr = "".join(f"line {i}\n" for i in range(30)) + "Uncaught Test262Error: last\n"
        _, detail = t262.decide(meta(), outcome(1, stdout="out\n", stderr=stderr), "node", 10)
        self.assertIn("stdout:\nout", detail)
        self.assertIn("Uncaught Test262Error: last", detail)
        self.assertNotIn("line 19\n", detail)
        self.assertEqual(t262.decide(meta(), outcome(), "node", 10), ("pass", ""))


class BoaTest(unittest.TestCase):
    def test_error_name_with_ansi_codes(self):
        self.assertEqual(
            t262.boa_error_name("\x1b[1;31mUncaught\x1b[0m: \x1b[31mSyntaxError\x1b[0m: unexpected token"),
            "SyntaxError",
        )
        self.assertEqual(t262.boa_error_name("Uncaught \x1b[33mTest262Error\x1b[0m: Expected"), "Test262Error")
        self.assertEqual(t262.boa_error_name("Uncaught: RangeError: x\nTypeError"), "RangeError")
        self.assertIsNone(t262.boa_error_name("Uncaught: \"error message\" ErrorX"))
        self.assertIsNone(t262.boa_error_name("Uncaught: MyErrors"))

    def test_boa_decision(self):
        neg = meta(negative={"phase": "parse", "type": "SyntaxError"})
        ansi = "\x1b[31mUncaught\x1b[0m \x1b[1mSyntaxError\x1b[0m: expected token ';'\n"
        self.assertEqual(t262.decide(neg, outcome(1, stderr=ansi), "boa", 10)[0], "pass")
        self.assertEqual(t262.decide(neg, outcome(0, stderr=ansi), "boa", 10)[0], "pass")
        wrong = ansi.replace("SyntaxError", "TypeError")
        self.assertEqual(t262.decide(neg, outcome(1, stderr=wrong), "boa", 10)[0], "fail")
        self.assertEqual(t262.decide(meta(), outcome(0, stderr=ansi), "boa", 10)[0], "fail")
        self.assertEqual(t262.decide(meta(), outcome(0), "boa", 10)[0], "pass")
        self.assertEqual(t262.decide(neg, outcome(0), "boa", 10)[0], "fail")


class OutputTest(unittest.TestCase):
    def test_paths(self):
        results = Path("/r")
        self.assertEqual(t262.output_path("boa", "0.22.0", False, False, results), Path("/r/boa/0.22.0.json"))
        self.assertEqual(t262.output_path("node", "26.13.0", True, False, results), Path("/r/control-node.json"))
        self.assertEqual(
            t262.output_path("vjs", "abcd", False, True, results), Path("/r/manual/vjs-abcd.json")
        )

    def test_control_mismatches(self):
        results = [
            {"file": "bench/test262/control/strict-pass.js", "mode": "strict", "result": "pass"},
            {"file": "bench/test262/control/strict-fail.js", "mode": "strict", "result": "fail"},
            {"file": "bench/test262/control/async-fail.js", "mode": "sloppy", "result": "pass"},
        ]
        self.assertEqual(
            t262.control_mismatches(results),
            ["bench/test262/control/async-fail.js (sloppy): pass, expected fail"],
        )

    def test_control_set_is_complete(self):
        control = BENCH / "test262" / "control"
        names = sorted(p.name for p in control.glob("*.js"))
        expected = sorted(
            f"{kind}-{expected_result}.js"
            for kind in ("strict", "nostrict", "module", "async", "negparse", "negruntime")
            for expected_result in ("pass", "fail")
        )
        self.assertEqual(names, expected)
        for name in names:
            with self.subTest(name=name):
                parsed = t262.parse_frontmatter((control / name).read_text(encoding="utf-8"))
                kind = name.split("-")[0]
                if kind == "strict":
                    self.assertEqual(t262.scenarios(parsed), ["strict"])
                elif kind == "nostrict":
                    self.assertEqual(t262.scenarios(parsed), ["sloppy"])
                elif kind == "module":
                    self.assertEqual(t262.scenarios(parsed), ["module"])
                elif kind == "async":
                    self.assertIn("async", parsed.flags)
                else:
                    phase = "parse" if kind == "negparse" else "runtime"
                    self.assertEqual(parsed.negative["phase"], phase)


if __name__ == "__main__":
    unittest.main()
