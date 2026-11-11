#!/usr/bin/env python3
"""Pinned revisions check (vault/CONTRACT.md §5.1 and §5.6).

  pins_check.py

Checks that bench/cache/wpt and bench/cache/test262 are at their pinned
commits, that the Servo and Boa downloads match their sha256, and that every
pinned binary reports its pinned version. Prints `FAIL <reason>` per problem.

Xvfb has no version flag (21.1.25 answers `-version` with "Unrecognized
option"); the version it reports is the vendor release of the X11 connection
setup, so the check starts it on a free display (-displayfd), connects to its
socket, decodes the release (12101025 -> 1.21.1.25) and stops it.
Exit codes: 0 with `OK` if everything matches, 77 if a package binary is not
installed (installing it needs sudo), 1 otherwise.
"""

import hashlib
import json
import os
import re
import select
import socket
import struct
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PINS = REPO / "bench" / "pins.json"
CACHE = Path("bench") / "cache"

VERSION_TIMEOUT_S = 60
X11_SOCKET_DIR = Path("/tmp/.X11-unix")


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


def decode_vendor_release(release):
    """X.Org vendor release number -> version, e.g. 12101025 -> "1.21.1.25"."""
    return "{}.{}.{}.{}".format(
        release // 10000000, release // 100000 % 100, release // 1000 % 100, release % 1000
    )


def setup_request():
    """X11 connection setup, little endian, protocol 11.0, no authorization."""
    return struct.pack("<cxHHHHxx", b"l", 11, 0, 0, 0)


def parse_setup_reply(data):
    """(vendor release, None) from an X11 setup reply, or (None, reason)."""
    if len(data) < 12:
        return None, f"short X11 setup reply ({len(data)} bytes)"
    if data[0] != 1:
        return None, f"X11 setup refused (status {data[0]})"
    return struct.unpack_from("<I", data, 8)[0], None


def read_display(fd, timeout):
    """Reads the display number Xvfb writes to -displayfd."""
    buf = b""
    while not buf.endswith(b"\n"):
        ready, _, _ = select.select([fd], [], [], timeout)
        if not ready:
            raise TimeoutError("Xvfb did not report its display")
        chunk = os.read(fd, 16)
        if not chunk:
            raise OSError("Xvfb exited before reporting its display")
        buf += chunk
    return int(buf)


def xvfb_version(binary, timeout=VERSION_TIMEOUT_S, socket_dir=X11_SOCKET_DIR):
    """Like run_version for Xvfb: (output, None) or (None, error)."""
    read_fd, write_fd = os.pipe()
    try:
        proc = subprocess.Popen(
            [str(binary), "-displayfd", str(write_fd), "-nolisten", "tcp"],
            pass_fds=(write_fd,),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except OSError as err:
        os.close(read_fd)
        os.close(write_fd)
        return None, str(err)
    os.close(write_fd)
    try:
        display = read_display(read_fd, timeout)
        with socket.socket(socket.AF_UNIX) as conn:
            conn.settimeout(timeout)
            conn.connect(str(Path(socket_dir) / f"X{display}"))
            conn.sendall(setup_request())
            data = b""
            while len(data) < 12:
                chunk = conn.recv(4096)
                if not chunk:
                    break
                data += chunk
        release, error = parse_setup_reply(data)
        if error:
            return None, error
        return f"X.Org vendor release {release} ({decode_vendor_release(release)})", None
    except (OSError, ValueError) as err:
        return None, f"cannot read the X11 vendor release: {err}"
    finally:
        os.close(read_fd)
        proc.kill()
        proc.wait()


def read_version(name, binary):
    if name == "xvfb":
        return xvfb_version(binary)
    return run_version(binary, "--version")


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
        output, error = read_version(name, binary)
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
