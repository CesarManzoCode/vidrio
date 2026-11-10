#!/usr/bin/env python3
"""Isolated WPT runner (vault/CONTRACT.md §5.5, §5.6).

  wptrun.py --product {firefox,chromium,servo,vidrio} (--all | --dir D... | --path T...)
            [--run N] [--force]
  wptrun.py --net-probe

One `./wpt run` per directory of bench/scope.json (--all), per --dir or per
--path, in series, with bench/cache/wpt as working directory. Each one writes
bench/results/wpt/<product>/<KEY>/<SLUG>.json (wptreport), <SLUG>.log (raw
mozlog), <SLUG>.mem (bytes of the cgroup memory.peak) and, for vidrio,
<SLUG>.shots (--log-wptscreenshot). SLUG is the path with "/" -> "__". KEY
is the pinned version for references or sha256[:16] of target/release/vidrio,
then "-" and the first 12 hex of the WPT commit, then "-rN" if --run N > 1.
A directory whose <SLUG>.json exists is skipped unless --force; a run that
exits non-zero or leaves no report or no memory.peak removes its <SLUG>.json
so that the next invocation retries it.

Isolation: every run is

  systemd-run --user --scope --quiet --collect -- unshare -rn sh -c \
    'ip link set lo up && exec unshare --map-user=<uid> --map-group=<gid> \
     <python> bench/wptrun.py --inner …'

so it has its own network namespace with only loopback, runs as the real
uid again, and gets a cgroup of its own. The --inner mode runs `./wpt run`,
waits, reads its cgroup from /proc/self/cgroup and writes
/sys/fs/cgroup/<path>/memory.peak to <SLUG>.mem. A peak above
run.memory_limit_bytes keeps the results but is reported and makes the exit
status 1.

Firefox gets --prefs-root with the profiles saved by bench/fetch.py: the
directory under bench/cache/firefox-prefs that holds profiles.json for the
pinned version (wptrunner reads <prefs-root>/profiles.json), or
bench/cache/firefox-prefs itself if that directory is not there.

--net-probe, in the same isolation, listens on 127.0.0.1 and connects to it,
tries 1.1.1.1:443 with a 3 s timeout and prints
`loopback=ok|fail external=blocked|reachable`; it exits 0 only for
`loopback=ok external=blocked`.

Exit status: 77 if a reference binary is missing (installing it needs sudo),
1 if target/release/vidrio is missing or a run failed, 0 otherwise.
"""

import argparse
import hashlib
import json
import os
import re
import shlex
import socket
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BENCH = REPO / "bench"
PINS = BENCH / "pins.json"
SCOPE = BENCH / "scope.json"
WPT = BENCH / "cache" / "wpt"
RESULTS = BENCH / "results" / "wpt"
SELF = BENCH / "wptrun.py"

PRODUCTS = ("firefox", "chromium", "servo", "vidrio")
EXIT_MISSING_REFERENCE = 77
PROBE_TIMEOUT_S = 3
PROBE_TARGET = ("1.1.1.1", 443)
PROBE_LINE = re.compile(r"^loopback=(ok|fail) external=(blocked|reachable)$")


class RunError(Exception):
    pass


def log(message):
    print(f"wptrun: {message}", flush=True)


def load_pins(path=PINS):
    return json.loads(Path(path).read_text())


def scope_dirs(path=SCOPE):
    return list(json.loads(Path(path).read_text())["wpt"])


def slug(test_path):
    return test_path.strip("/").replace("/", "__")


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def vidrio_binary(root=REPO):
    return Path(root) / "target" / "release" / "vidrio"


def reference_binaries(product, pins, root=REPO):
    """Binaries a reference product needs, as absolute paths."""
    if product == "firefox":
        names = ("firefox", "geckodriver")
    elif product == "chromium":
        names = ("chromium", "chromedriver")
    elif product == "servo":
        names = ("servo",)
    else:
        return []
    return [Path(root) / pins[name]["binary"] for name in names]


def run_key(product, pins, run=1, root=REPO):
    if product == "vidrio":
        base = sha256_file(vidrio_binary(root))[:16]
    else:
        base = pins[product]["version"]
    key = f"{base}-{pins['wpt']['commit'][:12]}"
    if run > 1:
        key += f"-r{run}"
    return key


def outputs(out_dir, test_path, product):
    name = slug(test_path)
    files = {
        "json": out_dir / f"{name}.json",
        "log": out_dir / f"{name}.log",
        "mem": out_dir / f"{name}.mem",
    }
    if product == "vidrio":
        files["shots"] = out_dir / f"{name}.shots"
    return files


def firefox_prefs_root(pins, root=REPO):
    base = Path(root) / "bench" / "cache" / "firefox-prefs"
    pinned = base / "profiles" / "stable" / pins["firefox"]["version"]
    return pinned if (pinned / "profiles.json").is_file() else base


def product_args(product, pins, root=REPO):
    root = Path(root)
    if product == "firefox":
        return [
            "--binary", str(root / pins["firefox"]["binary"]),
            "--webdriver-binary", str(root / pins["geckodriver"]["binary"]),
            "--prefs-root", str(firefox_prefs_root(pins, root)),
        ]
    if product == "chromium":
        args = [
            "--binary", str(root / pins["chromium"]["binary"]),
            "--webdriver-binary", str(root / pins["chromedriver"]["binary"]),
        ]
        for extra in pins["run"].get("chromium_extra_args", []):
            args += ["--binary-arg", extra]
        return args
    if product == "servo":
        return ["--binary", str(root / pins["servo"]["binary"])]
    if product == "vidrio":
        binary = str(vidrio_binary(root))
        return ["--binary", binary, "--webdriver-binary", binary]
    raise ValueError(f"unknown product {product}")


def wpt_command(product, test_path, files, pins, root=REPO):
    """The `./wpt run` command for one directory or path, run from the WPT tree."""
    root = Path(root)
    cmd = [
        "./wpt", "run",
        "--processes", str(pins["run"]["processes"]),
        "--headless",
        "--no-manifest-update",
        "--manifest", str(root / "bench" / "cache" / "wpt" / "MANIFEST.json"),
        "--no-fail-on-unexpected",
        "--log-wptreport", str(files["json"]),
        "--log-raw", str(files["log"]),
    ]
    if product == "vidrio":
        cmd += ["--log-wptscreenshot", str(files["shots"])]
    cmd += product_args(product, pins, root)
    cmd += [product, test_path.strip("/")]
    return cmd


def isolated(inner_args, uid=None, gid=None, python=None, script=SELF):
    """Wraps `<python> wptrun.py --inner <inner_args>` in the isolation of §5.5."""
    uid = os.getuid() if uid is None else uid
    gid = os.getgid() if gid is None else gid
    python = sys.executable if python is None else python
    inner = shlex.join([str(python), str(script), "--inner", *inner_args])
    shell = f"ip link set lo up && exec unshare --map-user={uid} --map-group={gid} {inner}"
    return [
        "systemd-run", "--user", "--scope", "--quiet", "--collect", "--",
        "unshare", "-rn", "sh", "-c", shell,
    ]


def cgroup_path(text):
    """The cgroup v2 path in the contents of /proc/self/cgroup."""
    for line in text.splitlines():
        if line.startswith("0::"):
            return line[3:]
    raise RunError("no cgroup v2 entry in /proc/self/cgroup")


def memory_peak():
    path = cgroup_path(Path("/proc/self/cgroup").read_text())
    return int(Path(f"/sys/fs/cgroup{path}/memory.peak").read_text().strip())


def probe_loopback():
    try:
        with socket.create_server(("127.0.0.1", 0)) as server:
            port = server.getsockname()[1]
            with socket.create_connection(("127.0.0.1", port), timeout=PROBE_TIMEOUT_S):
                conn, _ = server.accept()
                conn.close()
        return "ok"
    except OSError:
        return "fail"


def probe_external():
    try:
        with socket.create_connection(PROBE_TARGET, timeout=PROBE_TIMEOUT_S):
            return "reachable"
    except OSError:
        return "blocked"


def probe_line():
    return f"loopback={probe_loopback()} external={probe_external()}"


def parse_net_probe(text):
    """{"loopback": …, "external": …} from the probe output, or None."""
    for line in text.splitlines():
        match = PROBE_LINE.match(line.strip())
        if match:
            return {"loopback": match.group(1), "external": match.group(2)}
    return None


def inner_main(argv):
    """--inner: runs inside the isolation; never called directly by hand."""
    parser = argparse.ArgumentParser(prog="wptrun.py --inner")
    parser.add_argument("--net-probe", action="store_true")
    parser.add_argument("--mem")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.net_probe:
        print(probe_line(), flush=True)
        return 0
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command or not args.mem:
        parser.error("--mem FILE -- COMMAND… is required")
    returncode = subprocess.run(command, cwd=WPT).returncode
    try:
        peak = memory_peak()
    except (OSError, ValueError, RunError) as err:
        print(f"wptrun: memory.peak not read: {err}", file=sys.stderr, flush=True)
        return returncode or 1
    Path(args.mem).write_text(f"{peak}\n")
    return returncode


def net_probe():
    proc = subprocess.run(isolated(["--net-probe"]), capture_output=True, text=True)
    sys.stderr.write(proc.stderr)
    result = parse_net_probe(proc.stdout)
    if result is None:
        log(f"net probe produced no result (exit {proc.returncode})")
        return 1
    print(f"loopback={result['loopback']} external={result['external']}", flush=True)
    return 0 if result == {"loopback": "ok", "external": "blocked"} else 1


def run_one(product, test_path, out_dir, pins, force):
    """Runs one `./wpt run`. Returns "skipped", "ok" or "failed"."""
    files = outputs(out_dir, test_path, product)
    if files["json"].exists() and not force:
        log(f"{product} {test_path}: cached in {files['json']}")
        return "skipped"
    for path in files.values():
        if path.exists():
            path.unlink()
    out_dir.mkdir(parents=True, exist_ok=True)
    cmd = wpt_command(product, test_path, files, pins)
    log(f"{product} {test_path}: running")
    returncode = subprocess.run(isolated(["--mem", str(files["mem"]), "--", *cmd])).returncode
    if returncode != 0 or not files["json"].is_file() or not files["mem"].is_file():
        log(f"{product} {test_path}: FAIL (exit {returncode})")
        if files["json"].exists():
            files["json"].unlink()
        return "failed"
    peak = int(files["mem"].read_text().strip())
    limit = pins["run"]["memory_limit_bytes"]
    if peak > limit:
        log(f"{product} {test_path}: FAIL, memory.peak {peak} exceeds {limit}")
        return "failed"
    log(f"{product} {test_path}: done")
    return "ok"


def parse_args(argv):
    parser = argparse.ArgumentParser(description="Isolated WPT runner (CONTRACT §5.6).")
    parser.add_argument("--product", choices=PRODUCTS)
    targets = parser.add_mutually_exclusive_group()
    targets.add_argument("--all", action="store_true")
    targets.add_argument("--dir", nargs="+", metavar="D")
    targets.add_argument("--path", nargs="+", metavar="T")
    parser.add_argument("--run", type=int, default=1, metavar="N")
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--net-probe", action="store_true")
    args = parser.parse_args(argv)
    if not args.net_probe:
        if args.product is None:
            parser.error("--product is required")
        if not (args.all or args.dir or args.path):
            parser.error("one of --all, --dir or --path is required")
        if args.run < 1:
            parser.error("--run must be at least 1")
    return args


def check_binaries(product, pins, root=REPO):
    """Returns the exit status for missing binaries, or None if all are there."""
    if product == "vidrio":
        if not vidrio_binary(root).is_file():
            log(f"{vidrio_binary(root)} not found; build it with cargo build --release")
            return 1
        return None
    missing = [b for b in reference_binaries(product, pins, root) if not b.is_file()]
    if missing:
        log(f"missing reference binary: {', '.join(map(str, missing))}")
        return EXIT_MISSING_REFERENCE
    return None


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ["--inner"]:
        return inner_main(argv[1:])
    args = parse_args(argv)
    if args.net_probe:
        return net_probe()
    pins = load_pins()
    status = check_binaries(args.product, pins)
    if status is not None:
        return status
    if not (WPT / "MANIFEST.json").is_file():
        log(f"{WPT / 'MANIFEST.json'} not found; run python3 bench/fetch.py")
        return 1
    tests = scope_dirs() if args.all else (args.dir or args.path)
    out_dir = RESULTS / args.product / run_key(args.product, pins, args.run)
    results = [run_one(args.product, t, out_dir, pins, args.force) for t in tests]
    return 1 if "failed" in results else 0


if __name__ == "__main__":
    os.umask(0o022)
    sys.exit(main())
