#!/usr/bin/env python3
"""Dependency guards (vault/CONTRACT.md §2 and §5.6).

  deps.py ldd BIN...   every library ldd reports is in the allowed list
  deps.py forbidden    no forbidden name appears as a dependency

Each finding prints `FAIL <reason>` and the exit code is 1; with no findings
the last line is `OK` and the exit code is 0.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

ALLOWED_LIBS = frozenset(
    {
        "linux-vdso.so.1",
        "libc.so.6",
        "libm.so.6",
        "libpthread.so.0",
        "libdl.so.2",
        "ld-linux-x86-64.so.2",
        "libxcb.so.1",
        "libxcb-shm.so.0",
        "libXau.so.6",
        "libXdmcp.so.6",
    }
)

FORBIDDEN = (
    "v8",
    "quickjs",
    "spidermonkey",
    "mozjs",
    "javascriptcore",
    "servo",
    "html5ever",
    "stylo",
    "cssparser",
    "selectors",
    "webkit",
    "blink",
    "gecko",
    "taffy",
    "skia",
    "cairo",
    "harfbuzz",
    "freetype",
    "fontconfig",
    "curl",
    "openssl",
)

# Paths (relative to the root) that never hold product manifests or sources.
SKIP_PREFIXES = ((".git",), ("target",), ("bench", "cache"), ("bench", "results"))

DEP_SECTION = re.compile(r"^(?:.*\.)?(?:dev-|build-)?dependencies$")
DEP_TABLE = re.compile(r"^(?:.*\.)?(?:dev-|build-)?dependencies\.(.+)$")
LINK_ATTR = re.compile(r'#\s*!?\[\s*link\s*\([^\]]*?\bname\s*=\s*"([^"]*)"')
EXTERN_CRATE = re.compile(r"\bextern\s+crate\s+([A-Za-z0-9_]+)")
LINK_LIB = re.compile(r"cargo::?rustc-link-lib=([^\"\s)]+)")


def forbidden_in(name):
    """Returns the forbidden name contained in `name` (case-insensitive), or None."""
    lowered = name.lower()
    for bad in FORBIDDEN:
        if bad in lowered:
            return bad
    return None


# --- ldd ---------------------------------------------------------------------


def parse_ldd(text):
    """Returns (library basename, not_found) pairs from `ldd` output."""
    libs = []
    for line in text.splitlines():
        line = line.strip()
        if not line or "statically linked" in line:
            continue
        if "=>" in line:
            left, right = line.split("=>", 1)
            name = left.strip()
            missing = "not found" in right
        else:
            name = line.split()[0]
            missing = False
        libs.append((name.rsplit("/", 1)[-1], missing))
    return libs


def ldd_findings(binary, text):
    findings = []
    for name, missing in parse_ldd(text):
        if missing:
            findings.append(f"{binary}: {name} not found")
        elif name not in ALLOWED_LIBS:
            findings.append(f"{binary}: {name} is not an allowed library")
    return findings


def run_ldd(binaries):
    findings = []
    for binary in binaries:
        if not Path(binary).is_file():
            findings.append(f"{binary}: no such file")
            continue
        proc = subprocess.run(["ldd", binary], capture_output=True, text=True)
        out = proc.stdout + proc.stderr
        if "not a dynamic executable" in out:
            continue  # static ELF: needs no shared library
        if proc.returncode != 0 and "statically linked" not in out:
            findings.append(f"{binary}: ldd failed: {out.strip()}")
            continue
        findings.extend(ldd_findings(binary, proc.stdout))
    return findings


# --- forbidden ---------------------------------------------------------------


def _unquote(text):
    text = text.strip()
    if len(text) >= 2 and text[0] == text[-1] and text[0] in "\"'":
        return text[1:-1]
    return text


def cargo_toml_findings(path, text):
    """Forbidden names declared in dependency sections of a Cargo.toml."""
    findings = []
    in_deps = False  # inside [dependencies] and friends: keys are crate names
    table_dep = False  # inside [dependencies.NAME]: only `package` matters
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("["):
            header = line.strip("[]").strip()
            in_deps = bool(DEP_SECTION.match(header))
            table = DEP_TABLE.match(header)
            table_dep = bool(table)
            if table and (bad := forbidden_in(_unquote(table.group(1)))):
                findings.append(f"{path}:{lineno}: dependency on {bad}: [{header}]")
            continue
        if "=" not in line or not (in_deps or table_dep):
            continue
        key, value = (part.strip() for part in line.split("=", 1))
        names = []
        if in_deps:
            names.append(_unquote(key))
            package = re.search(r"\bpackage\s*=\s*[\"']([^\"']+)", value)
            if package:
                names.append(package.group(1))
        elif _unquote(key) == "package":
            names.append(_unquote(value))
        for name in names:
            if bad := forbidden_in(name):
                findings.append(f"{path}:{lineno}: dependency on {bad}: {name}")
    return findings


def cargo_lock_findings(path, text):
    findings = []
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if line.startswith("source ="):
            findings.append(f"{path}:{lineno}: third-party crate ({line})")
        elif line.startswith("name ="):
            name = _unquote(line.split("=", 1)[1])
            if bad := forbidden_in(name):
                findings.append(f"{path}:{lineno}: package {name} ({bad})")
    return findings


def rust_findings(path, text):
    findings = []
    for pattern, what in (
        (LINK_ATTR, "#[link(name)]"),
        (EXTERN_CRATE, "extern crate"),
        (LINK_LIB, "cargo:rustc-link-lib"),
    ):
        for match in pattern.finditer(text):
            if bad := forbidden_in(match.group(1)):
                lineno = text.count("\n", 0, match.start()) + 1
                findings.append(f"{path}:{lineno}: {what} {match.group(1)} ({bad})")
    return findings


def _walk(root, filename=None, suffix=None):
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root)
        if any(rel.parts[: len(prefix)] == prefix for prefix in SKIP_PREFIXES):
            continue
        if not path.is_file():
            continue
        if (filename and path.name == filename) or (suffix and path.suffix == suffix):
            yield path, rel


def forbidden_findings(root):
    root = Path(root)
    findings = []
    for path, rel in _walk(root, filename="Cargo.toml"):
        findings.extend(cargo_toml_findings(rel, path.read_text(encoding="utf-8")))
    for path, rel in _walk(root, filename="Cargo.lock"):
        findings.extend(cargo_lock_findings(rel, path.read_text(encoding="utf-8")))
    for path, rel in _walk(root, suffix=".rs"):
        if rel.parts[0] == "crates":
            findings.extend(rust_findings(rel, path.read_text(encoding="utf-8")))
    return findings


# --- main --------------------------------------------------------------------


def report(findings):
    for finding in findings:
        print(f"FAIL {finding}")
    if findings:
        return 1
    print("OK")
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    ldd = sub.add_parser("ldd", help="check ldd output of binaries")
    ldd.add_argument("binaries", nargs="+", metavar="BIN")
    forbidden = sub.add_parser("forbidden", help="look for forbidden dependencies")
    forbidden.add_argument("--root", type=Path, default=REPO, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if args.command == "ldd":
        return report(run_ldd(args.binaries))
    return report(forbidden_findings(args.root))


if __name__ == "__main__":
    sys.exit(main())
