#!/usr/bin/env python3
"""Fetch pinned trees and binaries into bench/cache/ (vault/CONTRACT.md §5.6).

  fetch.py

Uses the network; it is not part of the measurement. Idempotent: each step is
skipped when its result is already in place.

  1. WPT and test262: shallow fetch of the pinned commit, detached checkout.
  2. Servo and Boa: download, check sha256, extract Servo, make Boa executable.
  3. ./wpt environment (created by ./wpt itself) with every requirement that
     `./wpt run` installs for the reference products (see WPT_REQUIREMENTS),
     and MANIFEST.json.
  4. Firefox test prefs in bench/cache/firefox-prefs via tools/wpt/browser.py.
  5. MojoJS bindings for the pinned Chromium in bench/cache/mojojs via
     tools/wpt/browser.py, the same download `./wpt run chromium` does.
  6. OpenH264 GMP plugin for Firefox in <venv>/browsers/nightly, where `./wpt
     run firefox` looks for it (it asks to download it when missing), via
     tools/wpt/browser.py.
  7. <venv>/servo -> bench/cache/servo: wptrunner's servo.py looks for
     resources/wpt-prefs.json only in the WPT tree and in ./_venv3/servo, where
     `./wpt install servo browser` would extract Servo.
  8. bench/wpt/vidrio.patch applied to the WPT tree if not applied yet.

The measurement runs without network (CONTRACT §5.5), so everything `./wpt
run` would otherwise download on first use is fetched here.

A missing package binary (it needs sudo to install) only prints a warning.
Exits 1 if any step failed, 0 otherwise.
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PINS = REPO / "bench" / "pins.json"
CACHE = REPO / "bench" / "cache"
PATCH = REPO / "bench" / "wpt" / "vidrio.patch"
MOJOJS = CACHE / "mojojs"
TREES = ("wpt", "test262")
DOWNLOADS = ("servo", "boa")

PREFS_SCRIPT = """\
import logging, sys
sys.path.insert(0, ".")
from tools.wpt.browser import Firefox
logging.basicConfig(level=logging.INFO)
print(Firefox(logging.getLogger("fetch")).install_prefs(sys.argv[1], dest=sys.argv[2]))
"""

MOJOJS_SCRIPT = """\
import logging, sys
sys.path.insert(0, ".")
from tools.wpt.browser import Chromium
logging.basicConfig(level=logging.INFO)
print(Chromium(logging.getLogger("fetch")).install_mojojs(dest=sys.argv[2], browser_binary=sys.argv[1]))
"""

OPENH264_SCRIPT = """\
import logging, sys
sys.path.insert(0, ".")
from tools.wpt.browser import Firefox
logging.basicConfig(level=logging.INFO)
firefox = Firefox(logging.getLogger("fetch"))
binary_dir = firefox._get_browser_binary_dir(sys.argv[2], sys.argv[3])
print(firefox.install_openh264(binary_dir=binary_dir, binary=sys.argv[1], channel=sys.argv[3]))
"""

# `./wpt run` defaults to --channel nightly; Firefox looks for OpenH264 under
# <venv>/browsers/<channel>/gmp-gmpopenh264.
WPT_CHANNEL = "nightly"

# What `./wpt run` installs before running (tools/wpt/commands.json for `run`,
# tools/wpt/run.py setup_wptrunner): the base requirements, the product ones
# (Firefox and Chromium; Servo has none), the platform accessibility ones
# (aamtest is a default test type) and the WebTransport ones. Inside the
# isolation pip has no network, so they must already be satisfied.
WPT_REQUIREMENTS = (
    "tools/manifest/requirements.txt",
    "tools/wptrunner/requirements.txt",
    "tools/serve/requirements.txt",
    "tools/wptrunner/requirements_firefox.txt",
    "tools/wptrunner/requirements_chromium.txt",
    "tools/wptrunner/requirements_platform_accessibility.txt",
    "tools/webtransport/requirements.txt",
)


class FetchError(Exception):
    pass


def log(message):
    print(f"fetch: {message}", flush=True)


def git(tree, *args):
    proc = subprocess.run(["git", "-C", str(tree), *args], capture_output=True, text=True)
    if proc.returncode != 0:
        raise FetchError(f"git {' '.join(args)} in {tree}: {proc.stderr.strip()}")
    return proc.stdout.strip()


def tree_head(tree):
    proc = subprocess.run(
        ["git", "-C", str(tree), "rev-parse", "HEAD"], capture_output=True, text=True
    )
    return proc.stdout.strip() if proc.returncode == 0 else None


def ensure_tree(tree, repo, commit):
    """Puts `tree` at `commit`. Returns True if it fetched, False if already there."""
    tree = Path(tree)
    if not (tree / ".git").exists():
        tree.mkdir(parents=True, exist_ok=True)
        git(tree, "init", "-q")
    if tree_head(tree) == commit:
        return False
    git(tree, "fetch", "-q", "--depth", "1", repo, commit)
    git(tree, "checkout", "-q", "--detach", "FETCH_HEAD")
    head = tree_head(tree)
    if head != commit:
        raise FetchError(f"{tree} at {head} after checkout, expected {commit}")
    return True


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def ensure_download(url, sha256, dest):
    """Downloads `url` to `dest` unless it is already there with the right
    sha256. A download with the wrong sha256 is deleted, never used. Returns
    True if it downloaded."""
    dest = Path(dest)
    if dest.is_file() and sha256_file(dest) == sha256:
        return False
    dest.parent.mkdir(parents=True, exist_ok=True)
    partial = dest.with_name(dest.name + ".part")
    with urllib.request.urlopen(url) as response, open(partial, "wb") as out:
        shutil.copyfileobj(response, out)
    actual = sha256_file(partial)
    if actual != sha256:
        partial.unlink()
        raise FetchError(f"{url}: sha256 {actual} != {sha256}")
    partial.replace(dest)
    return True


def extract_servo(archive, binary):
    """Extracts the Servo tarball so that `binary` exists with its `resources/`
    directory next to it."""
    binary = Path(binary)
    target = binary.parent
    staging = target / ".extract"
    if staging.exists():
        shutil.rmtree(staging)
    staging.mkdir(parents=True)
    with tarfile.open(archive) as tar:
        tar.extractall(staging, filter="data")
    found = sorted(staging.rglob(binary.name), key=lambda p: len(p.parts))
    found = [p for p in found if p.is_file()]
    if not found:
        shutil.rmtree(staging)
        raise FetchError(f"{archive}: no {binary.name} inside")
    top = found[0].parent
    for item in top.iterdir():
        dest = target / item.name
        if dest.is_dir() and not dest.is_symlink():
            shutil.rmtree(dest)
        elif dest.exists() or dest.is_symlink():
            dest.unlink()
        item.rename(dest)
    shutil.rmtree(staging)
    if not (target / "resources").is_dir():
        raise FetchError(f"{archive}: no resources/ next to {binary.name}")
    binary.chmod(binary.stat().st_mode | 0o111)


def ensure_servo(entry, root=REPO):
    binary = root / entry["binary"]
    archive = binary.parent / entry["url"].rsplit("/", 1)[1]
    downloaded = ensure_download(entry["url"], entry["sha256"], archive)
    if downloaded or not binary.is_file() or not (binary.parent / "resources").is_dir():
        extract_servo(archive, binary)
        return True
    return False


def ensure_boa(entry, root=REPO):
    binary = root / entry["binary"]
    downloaded = ensure_download(entry["url"], entry["sha256"], binary)
    binary.chmod(binary.stat().st_mode | 0o111)
    return downloaded


def wpt(tree, *args):
    proc = subprocess.run([sys.executable, "wpt", *args], cwd=tree)
    if proc.returncode != 0:
        raise FetchError(f"./wpt {' '.join(args)} exited with {proc.returncode}")


def wpt_venv_python(tree):
    for python in sorted(Path(tree).glob("_venv*/bin/python")):
        return python
    raise FetchError(f"no ./wpt virtualenv in {tree}")


def requirements_command(python, tree):
    cmd = [str(python), "-m", "pip", "--disable-pip-version-check", "install", "--prefer-binary"]
    for rel in WPT_REQUIREMENTS:
        cmd += ["-r", str(Path(tree) / rel)]
    return cmd


def ensure_wpt_env(tree):
    # ./wpt creates its virtualenv (_venv3) on first use and installs the
    # requirements of the command; run and install cover tools/wpt/browser.py.
    wpt(tree, "run", "--help")
    wpt(tree, "install", "--help")
    proc = subprocess.run(requirements_command(wpt_venv_python(tree), tree), cwd=tree)
    if proc.returncode != 0:
        raise FetchError(f"pip install of the ./wpt run requirements exited with {proc.returncode}")


def ensure_manifest(tree, rebuild):
    manifest = Path(tree) / "MANIFEST.json"
    if manifest.is_file() and not rebuild:
        return False
    wpt(tree, "manifest", "--no-download", "--path", str(manifest))
    return True


def ensure_firefox_prefs(tree, binary, dest):
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    proc = subprocess.run(
        [str(wpt_venv_python(tree)), "-c", PREFS_SCRIPT, str(binary), str(dest)],
        cwd=tree,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise FetchError(f"install_prefs failed: {proc.stderr.strip()}")
    return proc.stdout.strip().splitlines()[-1]


def mojojs_gen(dest=MOJOJS):
    """The gen/ directory that `./wpt run chromium --mojojs-path` takes."""
    return Path(dest) / "mojojs" / "gen"


def ensure_mojojs(tree, binary, dest=MOJOJS):
    """Downloads the MojoJS bindings matching `binary` once. Returns True if it
    downloaded."""
    if (mojojs_gen(dest) / "DOWNLOADED_FROM").is_file():
        return False
    Path(dest).mkdir(parents=True, exist_ok=True)
    proc = subprocess.run(
        [str(wpt_venv_python(tree)), "-c", MOJOJS_SCRIPT, str(binary), str(dest)],
        cwd=tree,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0 or not (mojojs_gen(dest) / "DOWNLOADED_FROM").is_file():
        raise FetchError(f"install_mojojs failed: {proc.stderr.strip()[-300:]}")
    return True


def openh264_dir(tree):
    venv = wpt_venv_python(tree).parent.parent
    return venv / "browsers" / WPT_CHANNEL / "gmp-gmpopenh264"


def ensure_openh264(tree, binary):
    """Installs the OpenH264 plugin `./wpt run firefox` uses once. Returns True
    if it installed it."""
    target = openh264_dir(tree)
    if target.is_dir() and any(p.is_dir() for p in target.iterdir()):
        return False
    proc = subprocess.run(
        [str(wpt_venv_python(tree)), "-c", OPENH264_SCRIPT, str(binary),
         str(target.parent.parent.parent), WPT_CHANNEL],
        cwd=tree,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0 or not (target.is_dir() and any(p.is_dir() for p in target.iterdir())):
        raise FetchError(f"install_openh264 failed: {proc.stderr.strip()[-300:]}")
    return True


def ensure_servo_link(tree, servo_dir):
    """Makes <venv>/servo a symlink to `servo_dir`. Returns True if it changed."""
    link = wpt_venv_python(tree).parent.parent / "servo"
    target = Path(servo_dir).resolve()
    if link.is_symlink() and link.resolve() == target:
        return False
    if link.is_symlink() or link.is_file():
        link.unlink()
    elif link.exists():
        raise FetchError(f"{link} exists and is not a symlink")
    link.symlink_to(target, target_is_directory=True)
    return True


def patch_applied(tree, patch):
    proc = subprocess.run(
        ["git", "-C", str(tree), "apply", "--reverse", "--check", str(patch)],
        capture_output=True,
    )
    return proc.returncode == 0


def ensure_patch(tree, patch):
    """Applies `patch` unless it is absent or already applied. Returns True if
    it applied it."""
    patch = Path(patch)
    if not patch.is_file() or patch_applied(tree, patch):
        return False
    git(tree, "apply", str(patch))
    return True


def missing_package_binaries(pins):
    return [
        (name, entry)
        for name, entry in pins.items()
        if "package" in entry and not Path(entry["binary"]).is_file()
    ]


def main():
    pins = json.loads(PINS.read_text())
    failures = []

    def step(label, action):
        try:
            result = action()
        except (FetchError, OSError, tarfile.TarError) as err:
            failures.append(label)
            log(f"FAIL {label}: {err}")
            return None
        log(f"{label}: {'updated' if result else 'up to date'}")
        return result

    for name, entry in missing_package_binaries(pins):
        log(f"WARN {name}: {entry['binary']} not installed (pacman -S {entry['package'].split()[0]})")

    fetched = {}
    for name in TREES:
        entry = pins[name]
        fetched[name] = step(name, lambda: ensure_tree(CACHE / name, entry["repo"], entry["commit"]))
    step("servo", lambda: ensure_servo(pins["servo"]))
    step("boa", lambda: ensure_boa(pins["boa"]))

    wpt_tree = CACHE / "wpt"
    if tree_head(wpt_tree) == pins["wpt"]["commit"]:
        if step("wpt env", lambda: ensure_wpt_env(wpt_tree) or True) is not None:
            step("manifest", lambda: ensure_manifest(wpt_tree, bool(fetched["wpt"])))
            firefox = Path(pins["firefox"]["binary"])
            if firefox.is_file():
                prefs = step(
                    "firefox prefs",
                    lambda: ensure_firefox_prefs(wpt_tree, firefox, CACHE / "firefox-prefs"),
                )
                if prefs:
                    log(f"firefox prefs root: {prefs}")
                step("openh264", lambda: ensure_openh264(wpt_tree, firefox))
            else:
                log("WARN firefox prefs skipped: Firefox not installed")
            servo_dir = (REPO / pins["servo"]["binary"]).parent
            step("servo link", lambda: ensure_servo_link(wpt_tree, servo_dir))
            chromium = Path(pins["chromium"]["binary"])
            if chromium.is_file():
                step("mojojs", lambda: ensure_mojojs(wpt_tree, chromium))
            else:
                log("WARN mojojs skipped: Chromium not installed")
        step("vidrio.patch", lambda: ensure_patch(wpt_tree, PATCH))

    if failures:
        log(f"failed: {', '.join(failures)}")
        return 1
    return 0


if __name__ == "__main__":
    os.umask(0o022)
    sys.exit(main())
