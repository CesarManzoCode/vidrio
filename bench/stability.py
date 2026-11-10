#!/usr/bin/env python3
"""Stability of two WPT runs of one directory (vault/CONTRACT.md §5.6, §5.7, D-13).

  stability.py --product P --dir D

Compares bench/results/wpt/P/<KEY>/<SLUG>.json of the base run with the one
of <KEY>-r2 (KEY as bench/wptrun.py builds it) by (test, subtest): subtest
null stands for the status of the test itself. Prints every difference,
marked "flaky" if bench/flaky.json lists that (test, subtest) and "NEW"
otherwise. Exits 0 only if every difference is listed and the entries of
bench/flaky.json under D are at most 0.5% of the tests of D (the tests in
either report); 1 otherwise, also when a report is missing.
"""

import argparse
import importlib.util
import json
import sys
from pathlib import Path

BENCH = Path(__file__).resolve().parent
FLAKY = BENCH / "flaky.json"
PINS = BENCH / "pins.json"
RESULTS = BENCH / "results"

CAP = 0.005
PRODUCTS = ("firefox", "chromium", "servo", "vidrio")
MISSING = "MISSING"


def _load(name):
    spec = importlib.util.spec_from_file_location(name, BENCH / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ratio = _load("ratio")


def statuses(report):
    """{(test, subtest or None): status}."""
    out = {}
    for result in report.get("results", []):
        out[(result["test"], None)] = result.get("status")
        for subtest in result.get("subtests") or []:
            out[(result["test"], subtest["name"])] = subtest.get("status")
    return out


def differences(base, other):
    """[(test, subtest, status1, status2)] sorted, MISSING where absent."""
    a, b = statuses(base), statuses(other)
    diffs = []
    for key in a.keys() | b.keys():
        first, second = a.get(key, MISSING), b.get(key, MISSING)
        if first != second:
            diffs.append((key[0], key[1], first, second))
    return sorted(diffs, key=lambda d: (d[0], d[1] is not None, d[1] or ""))


def flaky_entries(flaky):
    return {(entry["test"], entry.get("subtest")) for entry in flaky["tests"]}


def in_directory(test, directory):
    return test.startswith("/" + directory.strip("/") + "/")


def test_count(base, other):
    return len({r["test"] for r in base.get("results", [])} | {r["test"] for r in other.get("results", [])})


def check(base, other, flaky, directory):
    """(exit status, output lines)."""
    listed = flaky_entries(flaky)
    lines = []
    unlisted = 0
    for test, subtest, first, second in differences(base, other):
        mark = "flaky" if (test, subtest) in listed else "NEW"
        unlisted += mark == "NEW"
        name = test if subtest is None else f"{test} :: {subtest}"
        lines.append(f"{mark}  {name}  {first} -> {second}")
    tests = test_count(base, other)
    in_dir = sum(1 for test, _ in listed if in_directory(test, directory))
    cap = CAP * tests
    over = in_dir > cap
    lines.append(
        f"{len(lines)} differences, {unlisted} not in flaky.json; "
        f"flaky.json has {in_dir} entries for {tests} tests (cap {cap:g})"
    )
    if over:
        lines.append("flaky.json exceeds the 0.5% cap")
    return (1 if unlisted or over else 0), lines


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--product", required=True, choices=PRODUCTS)
    parser.add_argument("--dir", required=True)
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(sys.argv[1:] if argv is None else argv)
    pins = ratio.load_json(PINS)
    reports = []
    for run in (1, 2):
        key = ratio.wpt_key(args.product, pins, run)
        if key is None:
            print("stability: target/release/vidrio not found", file=sys.stderr)
            return 1
        path = ratio.report_path(RESULTS, args.product, key, args.dir)
        report = ratio.load_report(path)
        if report is None:
            print(f"stability: {path} not found", file=sys.stderr)
            return 1
        reports.append(report)
    status, lines = check(reports[0], reports[1], ratio.load_json(FLAKY), args.dir)
    print("\n".join(lines))
    return status


if __name__ == "__main__":
    sys.exit(main())
