#!/usr/bin/env python3
"""WPT ratios and test262 pass rates (vault/CONTRACT.md §5.3, §5.6, D-12).

  ratio.py

Reads bench/results/wpt/<product>/<KEY>/<SLUG>.json for vidrio, firefox,
chromium and servo over the 26 directories of bench/scope.json, with KEY as
bench/wptrun.py builds it for the base run (pinned version for references,
sha256[:16] of target/release/vidrio for vidrio, then "-" and the first 12
hex of the WPT commit). A unit is a subtest with status PASS, or a test
without subtests whose status is PASS or OK; tests listed in
bench/excluded.json do not count for any product.

Writes bench/results/ratio.json:

  {"vs_firefox": {D: R, ..., "total": R}, "vs_chromium": {...},
   "servo_vs_firefox": {...}, "units": {product: {D: n, ..., "total": n}},
   "P": ..., "P_boa": ..., "keys": {...}}

R is the quotient of sums; it is null when a report is missing or the
denominator is 0 (for "total", when any report of either product is
missing). P and P_boa are passing scenarios / scenarios run in
bench/results/test262/<engine>/<KEY>.json (vjs: sha256[:16] of
target/release/vjs; boa: pinned version), or null if there is no report.
Prints a short table.
"""

import hashlib
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BENCH = REPO / "bench"
PINS = BENCH / "pins.json"
SCOPE = BENCH / "scope.json"
EXCLUDED = BENCH / "excluded.json"
RESULTS = BENCH / "results"
OUTPUT = RESULTS / "ratio.json"

PRODUCTS = ("vidrio", "firefox", "chromium", "servo")
RATIOS = (
    ("vs_firefox", "vidrio", "firefox"),
    ("vs_chromium", "vidrio", "chromium"),
    ("servo_vs_firefox", "servo", "firefox"),
)


def load_json(path):
    return json.loads(Path(path).read_text())


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def slug(test_path):
    return test_path.strip("/").replace("/", "__")


def wpt_key(product, pins, run=1, root=REPO):
    """KEY of bench/wptrun.py, or None if target/release/vidrio is missing."""
    if product == "vidrio":
        binary = Path(root) / "target" / "release" / "vidrio"
        if not binary.is_file():
            return None
        base = sha256_file(binary)[:16]
    else:
        base = pins[product]["version"]
    key = f"{base}-{pins['wpt']['commit'][:12]}"
    if run > 1:
        key += f"-r{run}"
    return key


def test262_key(engine, pins, root=REPO):
    """KEY of bench/test262.py, or None if target/release/vjs is missing."""
    if engine == "vjs":
        binary = Path(root) / "target" / "release" / "vjs"
        return sha256_file(binary)[:16] if binary.is_file() else None
    return pins[engine]["version"]


def excluded_tests(path=EXCLUDED):
    return {entry["test"] for entry in load_json(path)["tests"]}


def report_path(results, product, key, directory):
    return Path(results) / "wpt" / product / key / f"{slug(directory)}.json"


def load_report(path):
    path = Path(path)
    return load_json(path) if path.is_file() else None


def count_units(report, excluded=frozenset()):
    units = 0
    for result in report.get("results", []):
        if result["test"] in excluded:
            continue
        subtests = result.get("subtests") or []
        if subtests:
            units += sum(1 for s in subtests if s.get("status") == "PASS")
        elif result.get("status") in ("PASS", "OK"):
            units += 1
    return units


def product_units(results, product, key, dirs, excluded):
    """{D: units or None, "total": sum or None}; None marks a missing report."""
    units = {}
    for directory in dirs:
        report = None if key is None else load_report(report_path(results, product, key, directory))
        units[directory] = None if report is None else count_units(report, excluded)
    values = [units[d] for d in dirs]
    units["total"] = None if None in values else sum(values)
    return units


def ratio(numerator, denominator):
    if numerator is None or denominator is None or denominator == 0:
        return None
    return numerator / denominator


def ratios(num_units, den_units, dirs):
    return {d: ratio(num_units[d], den_units[d]) for d in [*dirs, "total"]}


def pass_rate(path):
    """Passing scenarios / scenarios run, or None without report or scenarios."""
    report = load_report(path)
    if report is None:
        return None
    scenarios = report.get("scenarios", [])
    if not scenarios:
        return None
    return sum(1 for s in scenarios if s["result"] == "pass") / len(scenarios)


def compute(dirs, excluded, wpt_keys, t262_keys, results=RESULTS):
    units = {p: product_units(results, p, wpt_keys.get(p), dirs, excluded) for p in PRODUCTS}
    out = {name: ratios(units[num], units[den], dirs) for name, num, den in RATIOS}
    out["units"] = units
    for field, engine in (("P", "vjs"), ("P_boa", "boa")):
        key = t262_keys.get(engine)
        path = Path(results) / "test262" / engine / f"{key}.json"
        out[field] = None if key is None else pass_rate(path)
    out["keys"] = {"wpt": dict(wpt_keys), "test262": dict(t262_keys)}
    return out


def fmt(value):
    if value is None:
        return "-"
    if isinstance(value, float):
        return f"{value:.4f}"
    return str(value)


def table(out, dirs):
    names = [name for name, _, _ in RATIOS]
    rows = [["directory", *names, *PRODUCTS]]
    for d in [*dirs, "total"]:
        rows.append([d, *(fmt(out[n][d]) for n in names), *(fmt(out["units"][p][d]) for p in PRODUCTS)])
    widths = [max(len(row[i]) for row in rows) for i in range(len(rows[0]))]
    lines = ["  ".join(cell.ljust(w) for cell, w in zip(row, widths)).rstrip() for row in rows]
    lines.append(f"P={fmt(out['P'])}  P_boa={fmt(out['P_boa'])}")
    return "\n".join(lines)


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv:
        print("usage: ratio.py", file=sys.stderr)
        return 2
    pins = load_json(PINS)
    dirs = list(load_json(SCOPE)["wpt"])
    wpt_keys = {p: wpt_key(p, pins) for p in PRODUCTS}
    t262_keys = {e: test262_key(e, pins) for e in ("vjs", "boa")}
    out = compute(dirs, excluded_tests(), wpt_keys, t262_keys)
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(json.dumps(out, indent=2, sort_keys=True) + "\n")
    print(table(out, dirs))
    return 0


if __name__ == "__main__":
    sys.exit(main())
