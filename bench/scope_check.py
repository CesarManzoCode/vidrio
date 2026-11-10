#!/usr/bin/env python3
"""Scope check (vault/CONTRACT.md §5.2 and §5.6).

  scope_check.py [--manifest PATH] [--scope PATH]

Every WPT directory in bench/scope.json (or its registered successor) must
hold at least one test of any type in MANIFEST.json. Prints `FAIL <dir>` per
empty directory and exits 1; otherwise prints `OK` and exits 0.
"""

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = REPO / "bench" / "cache" / "wpt" / "MANIFEST.json"
DEFAULT_SCOPE = REPO / "bench" / "scope.json"


def has_leaf(node):
    """True if the manifest subtree holds at least one test entry."""
    if isinstance(node, list):
        return len(node) > 0
    if isinstance(node, dict):
        return any(has_leaf(child) for child in node.values())
    return False


def dir_has_tests(manifest, directory):
    """Manifest format 8+: items -> test type -> nested folders -> leaves."""
    parts = [p for p in directory.split("/") if p]
    for tree in manifest.get("items", {}).values():
        node = tree
        for part in parts:
            if not isinstance(node, dict) or part not in node:
                node = None
                break
            node = node[part]
        if node is not None and has_leaf(node):
            return True
    return False


def scope_findings(manifest, scope):
    version = manifest.get("version")
    if not isinstance(version, int) or version < 8:
        return [f"manifest version {version!r} (need 8 or later)"]
    successors = scope.get("successors", {})
    findings = []
    for directory in scope.get("wpt", []) + scope.get("wpt_extra", []):
        if dir_has_tests(manifest, directory):
            continue
        successor = successors.get(directory)
        if successor is not None and dir_has_tests(manifest, successor):
            continue
        findings.append(directory)
    return findings


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--scope", type=Path, default=DEFAULT_SCOPE)
    args = parser.parse_args(argv)
    try:
        manifest = json.loads(args.manifest.read_text())
    except (OSError, ValueError) as err:
        print(f"FAIL cannot read {args.manifest}: {err}")
        return 1
    scope = json.loads(args.scope.read_text())
    findings = scope_findings(manifest, scope)
    for directory in findings:
        print(f"FAIL {directory}")
    if findings:
        return 1
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
