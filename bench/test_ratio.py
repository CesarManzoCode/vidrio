import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

BENCH = Path(__file__).resolve().parent


def _load(name):
    spec = importlib.util.spec_from_file_location(name, BENCH / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ratio = _load("ratio")

DIRS = ["dom", "css/css-color"]
KEYS = {
    "vidrio": "abc-77787fe5fe23",
    "firefox": "159.0.1-77787fe5fe23",
    "chromium": "156.0.8078.79-77787fe5fe23",
    "servo": "0.8.0-77787fe5fe23",
}


def subtests(*statuses):
    return [{"name": f"s{i}", "status": s} for i, s in enumerate(statuses)]


def report(*results):
    return {"results": list(results)}


class CountUnits(unittest.TestCase):
    def test_subtests_count_pass_only(self):
        r = report({"test": "/dom/a.html", "status": "OK", "subtests": subtests("PASS", "FAIL", "PASS", "TIMEOUT")})
        self.assertEqual(ratio.count_units(r), 2)

    def test_test_without_subtests(self):
        r = report(
            {"test": "/dom/ok.html", "status": "OK", "subtests": []},
            {"test": "/dom/pass.html", "status": "PASS"},
            {"test": "/dom/fail.html", "status": "FAIL", "subtests": []},
        )
        self.assertEqual(ratio.count_units(r), 2)

    def test_excluded_test_does_not_count(self):
        r = report(
            {"test": "/dom/a.html", "status": "OK", "subtests": subtests("PASS", "PASS")},
            {"test": "/dom/b.html", "status": "PASS"},
        )
        self.assertEqual(ratio.count_units(r, {"/dom/a.html"}), 1)


class Compute(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.results = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, product, directory, data):
        path = ratio.report_path(self.results, product, KEYS[product], directory)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data))

    def passing(self, n, prefix="/dom/"):
        return report(*({"test": f"{prefix}t{i}.html", "status": "PASS"} for i in range(n)))

    def test_ratios_are_quotient_of_sums(self):
        self.write("vidrio", "dom", self.passing(1))
        self.write("vidrio", "css/css-color", self.passing(3, "/css/css-color/"))
        self.write("firefox", "dom", self.passing(4))
        self.write("firefox", "css/css-color", self.passing(4, "/css/css-color/"))
        out = ratio.compute(DIRS, set(), KEYS, {}, self.results)
        self.assertEqual(out["vs_firefox"]["dom"], 0.25)
        self.assertEqual(out["vs_firefox"]["css/css-color"], 0.75)
        self.assertEqual(out["vs_firefox"]["total"], 0.5)
        self.assertEqual(out["units"]["vidrio"], {"dom": 1, "css/css-color": 3, "total": 4})

    def test_missing_report_gives_null(self):
        self.write("servo", "dom", self.passing(2))
        self.write("firefox", "dom", self.passing(4))
        self.write("firefox", "css/css-color", self.passing(4, "/css/css-color/"))
        out = ratio.compute(DIRS, set(), KEYS, {}, self.results)
        self.assertEqual(out["servo_vs_firefox"]["dom"], 0.5)
        self.assertIsNone(out["servo_vs_firefox"]["css/css-color"])
        self.assertIsNone(out["servo_vs_firefox"]["total"])
        self.assertIsNone(out["units"]["servo"]["css/css-color"])
        self.assertIsNone(out["vs_chromium"]["dom"])

    def test_zero_denominator_gives_null(self):
        for d in DIRS:
            self.write("vidrio", d, self.passing(1))
            self.write("chromium", d, report({"test": "/dom/x.html", "status": "FAIL"}))
        out = ratio.compute(DIRS, set(), KEYS, {}, self.results)
        self.assertIsNone(out["vs_chromium"]["dom"])
        self.assertIsNone(out["vs_chromium"]["total"])
        self.assertEqual(out["units"]["chromium"]["total"], 0)

    def test_missing_vidrio_binary_gives_null(self):
        self.write("firefox", "dom", self.passing(4))
        out = ratio.compute(DIRS, set(), {**KEYS, "vidrio": None}, {}, self.results)
        self.assertIsNone(out["vs_firefox"]["dom"])

    def test_pass_rates_from_scenarios(self):
        path = self.results / "test262" / "boa" / "0.22.0.json"
        path.parent.mkdir(parents=True)
        scenarios = [
            {"file": "test/a.js", "mode": "strict", "result": "pass", "detail": ""},
            {"file": "test/a.js", "mode": "sloppy", "result": "fail", "detail": "x"},
            {"file": "test/b.js", "mode": "module", "result": "pass", "detail": ""},
            {"file": "test/c.js", "mode": "raw", "result": "pass", "detail": ""},
        ]
        path.write_text(json.dumps({"engine": "boa", "scenarios": scenarios}))
        out = ratio.compute(DIRS, set(), KEYS, {"vjs": None, "boa": "0.22.0"}, self.results)
        self.assertEqual(out["P_boa"], 0.75)
        self.assertIsNone(out["P"])


class Table(unittest.TestCase):
    def test_table_lists_directories_and_rates(self):
        units = {p: {"dom": None, "total": None} for p in ratio.PRODUCTS}
        out = {name: {"dom": None, "total": None} for name, _, _ in ratio.RATIOS}
        out.update(units=units, P=0.5, P_boa=None)
        text = ratio.table(out, ["dom"])
        self.assertIn("dom", text)
        self.assertIn("P=0.5000  P_boa=-", text)


if __name__ == "__main__":
    unittest.main()
