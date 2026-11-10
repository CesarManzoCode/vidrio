#!/usr/bin/env python3
"""Pinned revisions check (vault/CONTRACT.md §5.1 and §5.6).

  pins_check.py

Checks that bench/cache/wpt and bench/cache/test262 are at their pinned
commits, that the Servo and Boa downloads match their sha256, and that every
pinned binary reports its pinned version. Prints `FAIL <reason>` per problem.
Exit codes: 0 with `OK` if everything matches, 77 if a package binary is not
installed (installing it needs sudo), 1 otherwise.
"""

import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PINS = REPO / "bench" / "pins.json"
CACHE = Path("bench") / "cache"

# Binaries whose version flag is not `--version`.
VERSION_FLAGS = {"xvfb": "-version"}
VERSION_TIMEOUT_S = 60


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def resolve(root, path):
    path = Path(path)
    return path if path.is_absolute() else Path(root) / path


def hashed_file(root, name, entry):
    """File the pinned sha256 covers: the archive for tarballs, else the binary."""
    if entry["url"].endswith((".tar.gz", ".tgz", ".tar.xz")):
        return Path(root) / CACHE / name / entry["url"].rsplit("/", 1)[1]
    return resolve(root, entry["binary"])


def tree_findings(pins, root):
    findings = []
    for name, entry in pins.items():
        if "commit" not in entry or "repo" not in entry:
            continue
        tree = Path(root) / CACHE / name
        proc = subprocess.run(
            ["git", "-C", str(tree), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
        )
        head = proc.stdout.strip() if proc.returncode == 0 else None
        if head is None:
            findings.append(f"{name}: no git tree at {tree}")
        elif head != entry["commit"]:
            findings.append(f"{name}: HEAD {head} != {entry['commit']}")
    return findings


def version_matches(expected, output):
    """`expected` appears as a whole version (not a prefix of a longer one)."""
    return re.search(r"(?<!\d)" + re.escape(expected) + r"(?!\.?\d)", output) is not None


def run_version(binary, flag):
    try:
        proc = subprocess.run(
            [str(binary), flag],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            text=True,
            errors="replace",
            timeout=VERSION_TIMEOUT_S,
        )
    except (OSError, subprocess.TimeoutExpired) as err:
        return None, str(err)
    return proc.stdout, None


def binary_findings(pins, root):
    """Returns (findings, missing package binaries)."""
    findings = []
    missing = []
    for name, entry in pins.items():
        if "binary" not in entry:
            continue
        binary = resolve(root, entry["binary"])
        downloaded = "url" in entry
        if not binary.is_file():
            if downloaded:
                findings.append(f"{name}: {binary} missing (run bench/fetch.py)")
            else:
                missing.append(f"{name}: {binary} not installed ({entry['package']})")
            continue
        if downloaded:
            hashed = hashed_file(root, name, entry)
            if not hashed.is_file():
                findings.append(f"{name}: {hashed} missing (run bench/fetch.py)")
            elif sha256_file(hashed) != entry["sha256"]:
                findings.append(f"{name}: sha256 of {hashed} != {entry['sha256']}")
        output, error = run_version(binary, VERSION_FLAGS.get(name, "--version"))
        if output is None:
            findings.append(f"{name}: cannot run {binary}: {error}")
            continue
        if downloaded:
            ok = entry["version_output"] in output
            expected = entry["version_output"]
        else:
            ok = version_matches(entry["version"], output)
            expected = entry["version"]
        if not ok:
            first = output.strip().splitlines()[0] if output.strip() else "(no output)"
            findings.append(f"{name}: version {first!r}, expected {expected!r}")
    return findings, missing


def main(root=REPO, pins_path=PINS):
    pins = json.loads(Path(pins_path).read_text())
    findings = tree_findings(pins, root)
    more, missing = binary_findings(pins, root)
    findings += more
    for reason in missing:
        print(f"MISSING {reason}")
    for reason in findings:
        print(f"FAIL {reason}")
    if missing:
        return 77
    if findings:
        return 1
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
