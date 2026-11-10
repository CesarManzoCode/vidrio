import importlib.util
import unittest
from pathlib import Path

BENCH = Path(__file__).resolve().parent


def _load(name):
    spec = importlib.util.spec_from_file_location(name, BENCH / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


stability = _load("stability")


def run(*flipped, tests=400):
    """Report of `tests` dom tests; the indices in `flipped` FAIL their subtest."""
    results = []
    for i in range(tests):
        status = "FAIL" if i in flipped else "PASS"
        results.append({"test": f"/dom/t{i}.html", "status": "OK", "subtests": [{"name": "a", "status": status}]})
    return {"results": results}


def flaky(*entries):
    return {"tests": [{"test": t, "subtest": s, "statuses": ["PASS", "FAIL"]} for t, s in entries]}


class Differences(unittest.TestCase):
    def test_identical_runs_pass(self):
        status, lines = stability.check(run(), run(), flaky(), "dom")
        self.assertEqual(status, 0)
        self.assertTrue(lines[0].startswith("0 differences"))

    def test_listed_difference_passes(self):
        status, lines = stability.check(run(), run(3), flaky(("/dom/t3.html", "a")), "dom")
        self.assertEqual(status, 0)
        self.assertEqual(lines[0], "flaky  /dom/t3.html :: a  PASS -> FAIL")

    def test_unlisted_difference_fails(self):
        status, lines = stability.check(run(), run(3, 5), flaky(("/dom/t3.html", "a")), "dom")
        self.assertEqual(status, 1)
        self.assertIn("NEW  /dom/t5.html :: a  PASS -> FAIL", lines)

    def test_test_status_and_missing_subtest(self):
        base = {"results": [{"test": "/dom/x.html", "status": "OK", "subtests": [{"name": "a", "status": "PASS"}]}]}
        other = {"results": [{"test": "/dom/x.html", "status": "TIMEOUT", "subtests": []}]}
        diffs = stability.differences(base, other)
        self.assertEqual(
            diffs,
            [("/dom/x.html", None, "OK", "TIMEOUT"), ("/dom/x.html", "a", "PASS", stability.MISSING)],
        )

    def test_list_over_cap_fails(self):
        # 400 tests: the cap is 2 entries; three listed differences exceed it.
        entries = [(f"/dom/t{i}.html", "a") for i in (1, 2, 3)]
        status, lines = stability.check(run(), run(1, 2, 3), flaky(*entries), "dom")
        self.assertEqual(status, 1)
        self.assertEqual(lines[-1], "flaky.json exceeds the 0.5% cap")

    def test_cap_counts_only_entries_of_the_directory(self):
        entries = [("/dom/t1.html", "a"), ("/css/x.html", None), ("/css/y.html", None)]
        status, _ = stability.check(run(), run(1), flaky(*entries), "dom")
        self.assertEqual(status, 0)


if __name__ == "__main__":
    unittest.main()
