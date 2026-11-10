#!/usr/bin/env python3
"""Own test262 runner, the same for every engine (vault/CONTRACT.md §5.4, §5.6).

  test262.py --engine {node,boa,vjs} [--control] [--force] [--threads N] [--filter PREFIX]

Applies bench/cache/test262/INTERPRETING.md:

  - Walks test/language, test/built-ins and test/annexB (or, with --control,
    bench/test262/control/), skipping `_FIXTURE` files, and reads the YAML
    frontmatter (includes, flags, features, negative with phase and type).
  - Skips files with a feature listed before `## Standard language features`
    in features.txt, and files flagged CanBlockIsFalse: the main thread of the
    three engines can block (Boa runs without --no-can-block).
  - Scenarios: raw -> unmodified; module -> module; onlyStrict -> strict;
    noStrict -> sloppy; otherwise sloppy and strict. Strict mode is
    `"use strict";` as the first line of the final code.
  - Prelude (never for raw): harness/assert.js, harness/sta.js,
    harness/doneprintHandle.js for async tests, then each include.
  - The prepared code is written as a temporary file next to the original
    test, so that relative imports (static or dynamic) resolve, and removed
    afterwards. A module test that is imported by its own name (from itself
    or from a fixture) runs instead in a temporary sibling directory holding
    links to the other files of its directory and the prepared code under
    the original name, so the self-import is the same module, as in
    INTERPRETING.md.

Decision, the same for every engine: a non-negative scenario passes if the
process exits with 0 without an uncaught error and, if async, prints
Test262:AsyncTestComplete and no Test262:AsyncTestFailure. A negative one
passes if it ends with an uncaught error named negative.type; parse and
resolution negatives start with $DONOTEVALUATE(), so reaching it throws and
fails. Exceeding run.test262_timeout_s of bench/pins.json fails.

Engine adapters only invoke and extract the error name: Node and vjs print
`Uncaught <Name>: <message>` to stderr and exit with 1; for Boa the name is
the first identifier ending in `Error` once ANSI codes are removed.

Writes bench/results/test262/<engine>/<KEY>.json (control:
bench/results/test262/control-<engine>.json) unless it exists and --force is
not given. --filter keeps the files whose path starts with PREFIX; it is for
manual use, always runs, and writes to bench/results/test262/manual/.
With --control, exits 1 if a `-pass` file fails or a `-fail` file passes.
"""

import argparse
import concurrent.futures
import dataclasses
import hashlib
import itertools
import json
import os
import re
import shutil
import subprocess
import sys
import threading
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BENCH = REPO / "bench"
PINS = BENCH / "pins.json"
TEST262 = BENCH / "cache" / "test262"
CONTROL_DIR = "bench/test262/control"
NODE_HOST = BENCH / "test262" / "node_host.js"
RESULTS = BENCH / "results" / "test262"
VJS = REPO / "target" / "release" / "vjs"

ENGINES = ("node", "boa", "vjs")
TEST_DIRS = ("test/language", "test/built-ins", "test/annexB")
STANDARD_SECTION = "## Standard language features"
EXCLUDED_FLAGS = ("CanBlockIsFalse",)
TEMP_MARK = ".vidrio-tmp-"
DETAIL_LINES = 10
ASYNC_COMPLETE = "Test262:AsyncTestComplete"
ASYNC_FAILURE = "Test262:AsyncTestFailure"

FRONTMATTER = re.compile(r"/\*---(.*?)---\*/", re.S)
TOP_KEY = re.compile(r"^([A-Za-z_][\w-]*):(.*)$")
LIST_ITEM = re.compile(r"^\s*-\s*(.*?)\s*$")
MAP_ITEM = re.compile(r"^\s*([A-Za-z_]\w*):\s*(.*?)\s*$")
UNCAUGHT_LINE = re.compile(r"^Uncaught ([^\s:]+)", re.M)
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
BOA_ERROR_NAME = re.compile(r"(?<![\w$])([A-Za-z_$][\w$]*Error)(?![\w$])")


@dataclasses.dataclass
class Meta:
    includes: list = dataclasses.field(default_factory=list)
    flags: list = dataclasses.field(default_factory=list)
    features: list = dataclasses.field(default_factory=list)
    negative: dict = None


@dataclasses.dataclass
class Outcome:
    exit_code: int
    stdout: str
    stderr: str
    timed_out: bool = False


def read_text(path):
    with open(path, encoding="utf-8", newline="") as f:
        return f.read()


def write_text(path, text):
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text)


# --- Frontmatter -----------------------------------------------------------


def _scalar(text):
    text = re.sub(r"\s+#.*$", "", text.strip())
    if len(text) >= 2 and text[0] == text[-1] and text[0] in "'\"":
        return text[1:-1]
    return text


def _flow_items(text):
    inner = text.strip()[1:]
    if "]" in inner:
        inner = inner[: inner.rindex("]")]
    return [_scalar(item) for item in inner.split(",") if item.strip()]


def _frontmatter_fields(body):
    """Top-level keys of the frontmatter -> (inline value, continuation lines)."""
    fields = {}
    lines = body.splitlines()
    i = 0
    while i < len(lines):
        match = TOP_KEY.match(lines[i])
        i += 1
        if not match:
            continue
        block = []
        while i < len(lines) and (
            not lines[i].strip() or lines[i][0] in " \t" or lines[i].startswith("-")
        ):
            block.append(lines[i])
            i += 1
        fields[match.group(1)] = (match.group(2).strip(), block)
    return fields


def _list_field(fields, key):
    if key not in fields:
        return []
    inline, block = fields[key]
    if inline.startswith("["):
        return _flow_items(" ".join([inline] + [line.strip() for line in block]))
    if inline:
        return [_scalar(inline)]
    items = []
    for line in block:
        match = LIST_ITEM.match(line)
        if match and match.group(1):
            items.append(_scalar(match.group(1)))
    return items


def _map_field(fields, key):
    inline, block = fields[key]
    entries = {}
    if inline.startswith("{"):
        for part in inline.strip("{} ").split(","):
            name, _, value = part.partition(":")
            entries[name.strip()] = _scalar(value)
    for line in block:
        match = MAP_ITEM.match(line)
        if match:
            entries[match.group(1)] = _scalar(match.group(2))
    return entries


def parse_frontmatter(source):
    """Reads the `/*--- ... ---*/` YAML block: includes, flags, features and
    negative ({"phase", "type"}). Only the subset of YAML that test262 uses
    for those keys (flow and block lists, a one-level mapping)."""
    meta = Meta()
    match = FRONTMATTER.search(source)
    if not match:
        return meta
    fields = _frontmatter_fields(match.group(1))
    meta.includes = _list_field(fields, "includes")
    meta.flags = _list_field(fields, "flags")
    meta.features = _list_field(fields, "features")
    if "negative" in fields:
        negative = _map_field(fields, "negative")
        meta.negative = {"phase": negative.get("phase"), "type": negative.get("type")}
    return meta


# --- Selection and scenarios -----------------------------------------------


def excluded_features(features_txt):
    """Every non-empty, non-comment line of features.txt before the
    `## Standard language features` section (vault/CONTRACT.md D-10)."""
    excluded = []
    for line in features_txt.splitlines():
        text = line.strip()
        if text.startswith(STANDARD_SECTION):
            break
        if text and not text.startswith("#"):
            excluded.append(text)
    return excluded


def skip_reason(meta, excluded):
    for flag in EXCLUDED_FLAGS:
        if flag in meta.flags:
            return f"flag {flag}"
    for feature in meta.features:
        if feature in excluded:
            return f"feature {feature}"
    return None


def scenarios(meta):
    flags = meta.flags
    if "raw" in flags:
        return ["raw"]
    if "module" in flags:
        return ["module"]
    if "onlyStrict" in flags:
        return ["strict"]
    if "noStrict" in flags:
        return ["sloppy"]
    return ["sloppy", "strict"]


def prelude_names(meta):
    names = ["assert.js", "sta.js"]
    if "async" in meta.flags:
        names.append("doneprintHandle.js")
    for name in meta.includes:
        if name not in names:
            names.append(name)
    return names


def build_source(meta, source, mode, harness):
    """Final code of one scenario. `harness(name)` returns the text of
    harness/<name>."""
    if mode == "raw":
        return source
    parts = ['"use strict";\n'] if mode == "strict" else []
    parts.extend(harness(name) + "\n" for name in prelude_names(meta))
    parts.append(source)
    return "".join(parts)


def discover(root, dirs, prefix=None):
    """(path relative to root, absolute path) of every test file, sorted."""
    root = Path(root)
    for directory in dirs:
        for dirpath, dirnames, filenames in os.walk(root / directory):
            dirnames[:] = sorted(d for d in dirnames if not d.startswith(TEMP_MARK))
            for name in sorted(filenames):
                if not name.endswith(".js") or "_FIXTURE" in name or TEMP_MARK in name:
                    continue
                path = Path(dirpath) / name
                rel = path.relative_to(root).as_posix()
                if prefix and not rel.startswith(prefix):
                    continue
                yield rel, path


def imports_itself(name, source, fixtures):
    """True if `./<name>` is imported by the test itself or by a fixture of
    its directory."""
    pattern = re.compile(r"""['"]\./""" + re.escape(name) + r"""['"]""")
    return bool(pattern.search(source)) or any(pattern.search(text) for text in fixtures)


# --- Engines ---------------------------------------------------------------


def strip_ansi(text):
    return ANSI.sub("", text)


def boa_error_name(text):
    match = BOA_ERROR_NAME.search(strip_ansi(text))
    return match.group(1) if match else None


def uncaught_error(engine, outcome):
    """(the scenario ended with an uncaught error, its name or None)."""
    if engine == "boa":
        stderr = strip_ansi(outcome.stderr)
        stdout = "\n".join(
            line
            for line in strip_ansi(outcome.stdout).splitlines()
            if not line.startswith("Test262:")
        )
        name = boa_error_name(stderr)
        if name is None and "Uncaught" in stdout:
            name = boa_error_name(stdout)
        errored = (
            outcome.exit_code != 0
            or name is not None
            or "Uncaught" in stderr
            or "Uncaught" in stdout
        )
        return errored, name
    match = UNCAUGHT_LINE.search(outcome.stderr)
    name = match.group(1) if match and outcome.exit_code == 1 else None
    return outcome.exit_code != 0 or match is not None, name


def command(engine, binary, path, mode):
    if engine == "node":
        cmd = [str(binary), "--experimental-vm-modules", "--expose-gc", str(NODE_HOST)]
        if mode == "module":
            cmd.append("--module")
    elif engine == "boa":
        cmd = [str(binary), "--test262-object"]
        if mode == "strict":
            cmd.append("--strict")
        if mode == "module":
            cmd.append("-m")
    else:
        cmd = [str(binary), "--test262"]
        if mode == "strict":
            cmd.append("--strict")
        if mode == "module":
            cmd.append("--module")
    cmd.append(str(path))
    return cmd


# --- Decision --------------------------------------------------------------


def _tail(text):
    return "\n".join(text.rstrip("\n").splitlines()[-DETAIL_LINES:])


def failure_detail(reason, outcome):
    parts = [reason]
    if outcome.stdout.strip():
        parts.append("stdout:\n" + _tail(strip_ansi(outcome.stdout)))
    if outcome.stderr.strip():
        parts.append("stderr:\n" + _tail(strip_ansi(outcome.stderr)))
    return "\n".join(parts)


def decide(meta, outcome, engine, timeout):
    """("pass" | "fail", detail); detail is empty for a pass."""
    if outcome.timed_out:
        return "fail", failure_detail(f"timeout after {timeout} s", outcome)
    errored, name = uncaught_error(engine, outcome)
    if meta.negative:
        expected = meta.negative.get("type")
        phase = meta.negative.get("phase")
        if errored and name == expected:
            return "pass", ""
        if not errored:
            reason = f"expected uncaught {expected} ({phase}), completed without error"
        else:
            got = name or "an error without name"
            reason = f"expected uncaught {expected} ({phase}), got {got} (exit {outcome.exit_code})"
        return "fail", failure_detail(reason, outcome)
    if errored:
        reason = f"uncaught {name}" if name else "uncaught error"
        return "fail", failure_detail(f"{reason} (exit {outcome.exit_code})", outcome)
    if "async" in meta.flags:
        lines = [line.strip() for line in outcome.stdout.splitlines()]
        if any(line.startswith(ASYNC_FAILURE) for line in lines):
            return "fail", failure_detail("async test reported failure", outcome)
        if ASYNC_COMPLETE not in lines:
            return "fail", failure_detail(f"async test did not print {ASYNC_COMPLETE}", outcome)
    return "pass", ""


# --- Running ---------------------------------------------------------------


class Harness:
    """Cached reader of harness/ files, shared by the worker threads."""

    def __init__(self, root):
        self.root = Path(root) / "harness"
        self.cache = {}
        self.lock = threading.Lock()

    def __call__(self, name):
        with self.lock:
            text = self.cache.get(name)
            if text is None:
                text = read_text(self.root / name)
                self.cache[name] = text
        return text


class Fixtures:
    """Cached texts of the `_FIXTURE` files of each directory."""

    def __init__(self):
        self.cache = {}
        self.lock = threading.Lock()

    def __call__(self, directory):
        with self.lock:
            texts = self.cache.get(directory)
            if texts is None:
                texts = [
                    read_text(p)
                    for p in sorted(Path(directory).iterdir())
                    if p.is_file() and "_FIXTURE" in p.name and p.suffix in (".js", ".json")
                ]
                self.cache[directory] = texts
        return texts


_serial = itertools.count()


def _temp_name(stem, mode):
    return f"{stem}{TEMP_MARK}{mode}-{os.getpid()}-{next(_serial)}.js"


def _make_mirror(path, code):
    """Temporary sibling directory with links to the other files of the test's
    directory and `code` under the test's own name."""
    mirror = path.parent / f"{TEMP_MARK}{os.getpid()}-{next(_serial)}"
    mirror.mkdir()
    for sibling in path.parent.iterdir():
        if sibling.name == path.name or TEMP_MARK in sibling.name or not sibling.is_file():
            continue
        try:
            os.link(sibling, mirror / sibling.name)
        except OSError:
            shutil.copyfile(sibling, mirror / sibling.name)
    write_text(mirror / path.name, code)
    return mirror, mirror / path.name


def _text(data):
    if data is None:
        return ""
    if isinstance(data, bytes):
        return data.decode("utf-8", "replace")
    return data


def execute(cmd, cwd, timeout):
    try:
        proc = subprocess.run(
            cmd, cwd=cwd, stdin=subprocess.DEVNULL, capture_output=True, timeout=timeout
        )
    except subprocess.TimeoutExpired as err:
        return Outcome(None, _text(err.stdout), _text(err.stderr), timed_out=True)
    return Outcome(proc.returncode, _text(proc.stdout), _text(proc.stderr))


@dataclasses.dataclass
class Context:
    engine: str
    binary: Path
    timeout: int
    harness: Harness
    fixtures: Fixtures


def run_scenario(ctx, rel, path, meta, mode):
    source = read_text(path)
    code = build_source(meta, source, mode, ctx.harness)
    cleanup = None
    if mode == "raw":
        target = path
    elif mode == "module" and imports_itself(path.name, source, ctx.fixtures(path.parent)):
        mirror, target = _make_mirror(path, code)
        cleanup = lambda: shutil.rmtree(mirror, ignore_errors=True)  # noqa: E731
    else:
        target = path.parent / _temp_name(path.stem, mode)
        write_text(target, code)
        cleanup = lambda: target.unlink(missing_ok=True)  # noqa: E731
    try:
        outcome = execute(command(ctx.engine, ctx.binary, target, mode), target.parent, ctx.timeout)
    finally:
        if cleanup:
            cleanup()
    result, detail = decide(meta, outcome, ctx.engine, ctx.timeout)
    return {"file": rel, "mode": mode, "result": result, "detail": detail}


def plan(tests, excluded):
    """Scenarios to run (rel, path, meta, mode) and the number of skipped files."""
    jobs = []
    skipped = 0
    for rel, path in tests:
        meta = parse_frontmatter(read_text(path))
        if skip_reason(meta, excluded):
            skipped += 1
            continue
        jobs.extend((rel, path, meta, mode) for mode in scenarios(meta))
    return jobs, skipped


def control_mismatches(results):
    mismatches = []
    for scenario in results:
        expected = "pass" if Path(scenario["file"]).stem.endswith("-pass") else "fail"
        if scenario["result"] != expected:
            mismatches.append(
                f"{scenario['file']} ({scenario['mode']}): {scenario['result']}, expected {expected}"
            )
    return mismatches


def output_path(engine, key, control, filtered, results=RESULTS):
    if filtered:
        return results / "manual" / f"{'control-' if control else ''}{engine}-{key}.json"
    if control:
        return results / f"control-{engine}.json"
    return results / engine / f"{key}.json"


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


class SetupError(Exception):
    def __init__(self, message, code=1):
        super().__init__(message)
        self.code = code


def engine_setup(engine, pins):
    """(binary, engine_version, KEY) of an engine."""
    if engine == "node":
        binary = Path(pins["node"]["binary"])
        if not binary.is_file():
            raise SetupError(f"{binary} not installed (pacman -S nodejs)", 77)
        return binary, pins["node"]["version"], pins["node"]["version"]
    if engine == "boa":
        binary = REPO / pins["boa"]["binary"]
        if not binary.is_file():
            raise SetupError(f"{binary} missing (run python3 bench/fetch.py)")
        return binary, pins["boa"]["version"], pins["boa"]["version"]
    if not VJS.is_file():
        raise SetupError(f"{VJS} missing (run cargo build --release)")
    key = sha256_file(VJS)[:16]
    return VJS, key, key


def tree_head(tree):
    proc = subprocess.run(
        ["git", "-C", str(tree), "rev-parse", "HEAD"], capture_output=True, text=True
    )
    return proc.stdout.strip() if proc.returncode == 0 else None


def parse_args(argv):
    parser = argparse.ArgumentParser(description="Run test262 on one engine.")
    parser.add_argument("--engine", required=True, choices=ENGINES)
    parser.add_argument("--control", action="store_true", help="run bench/test262/control/")
    parser.add_argument("--force", action="store_true", help="run even if the result exists")
    parser.add_argument("--threads", type=int, help="default: run.test262_threads")
    parser.add_argument("--filter", metavar="PREFIX", help="manual use: only paths with PREFIX")
    return parser.parse_args(argv)


def log(message):
    print(f"test262: {message}", file=sys.stderr, flush=True)


def main(argv=None):
    args = parse_args(argv)
    pins = json.loads(PINS.read_text())
    threads = args.threads or pins["run"]["test262_threads"]
    timeout = pins["run"]["test262_timeout_s"]
    try:
        if not (TEST262 / "INTERPRETING.md").is_file():
            raise SetupError(f"{TEST262} missing (run python3 bench/fetch.py)")
        commit = tree_head(TEST262)
        if commit != pins["test262"]["commit"]:
            raise SetupError(f"{TEST262} at {commit}, expected {pins['test262']['commit']}")
        binary, version, key = engine_setup(args.engine, pins)
    except SetupError as err:
        log(str(err))
        return err.code

    out = output_path(args.engine, key, args.control, bool(args.filter))
    if out.exists() and not args.force and not args.filter:
        log(f"{out.relative_to(REPO)} exists (use --force to run again)")
        return 0

    excluded = excluded_features(read_text(TEST262 / "features.txt"))
    if args.control:
        tests = discover(REPO, (CONTROL_DIR,), args.filter)
    else:
        tests = discover(TEST262, TEST_DIRS, args.filter)
    jobs, skipped = plan(tests, excluded)
    ctx = Context(args.engine, binary, timeout, Harness(TEST262), Fixtures())
    log(f"{args.engine}: {len(jobs)} scenarios, {skipped} files skipped, {threads} threads")

    results = []
    errors = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=threads) as pool:
        futures = {pool.submit(run_scenario, ctx, *job): job for job in jobs}
        for done, future in enumerate(concurrent.futures.as_completed(futures), 1):
            try:
                results.append(future.result())
            except OSError as err:
                rel, _, _, mode = futures[future]
                errors.append(f"{rel} ({mode}): {err}")
            if done % 5000 == 0:
                log(f"{done}/{len(jobs)}")
    if errors:
        for error in errors:
            log(f"runner error: {error}")
        log(f"{len(errors)} runner errors; no result written")
        return 1

    results.sort(key=lambda s: (s["file"], s["mode"]))
    report = {
        "engine": args.engine,
        "engine_version": version,
        "test262_commit": commit,
        "excluded_features": excluded,
        "excluded_flags": list(EXCLUDED_FLAGS),
        "scenarios": results,
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    partial = out.with_name(out.name + ".part")
    partial.write_text(json.dumps(report, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    partial.replace(out)
    passed = sum(1 for s in results if s["result"] == "pass")
    print(f"test262: {args.engine} {passed}/{len(results)} scenarios pass -> {out.relative_to(REPO)}")

    if args.control:
        mismatches = control_mismatches(results)
        for mismatch in mismatches:
            log(f"control mismatch: {mismatch}")
        if mismatches:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
