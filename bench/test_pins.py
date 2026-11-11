import contextlib
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import shutil
import struct
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

BENCH = Path(__file__).resolve().parent


def _load(name):
    spec = importlib.util.spec_from_file_location(name, BENCH / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


scope_check = _load("scope_check")
pins_check = _load("pins_check")
fetch = _load("fetch")

LEAF = [["0123abcd", [None, {}]]]
LEAF_URL = [["4567ef01", ["/dom/a.html?x", {}]]]


def synthetic_manifest(scope):
    """Manifest with one testharness test in every scope directory, except
    html/syntax/serializing-html-fragments, which holds a reftest."""
    testharness = {}
    reftest = {}
    for directory in scope["wpt"] + scope["wpt_extra"]:
        tree = reftest if directory.endswith("serializing-html-fragments") else testharness
        node = tree
        for part in directory.split("/"):
            node = node.setdefault(part, {})
        node["a.html"] = LEAF_URL if directory == "dom" else LEAF
    return {"version": 9, "url_base": "/", "items": {"testharness": testharness, "reftest": reftest}}


class ScopeCheckTest(unittest.TestCase):
    def setUp(self):
        self.scope = json.loads((BENCH / "scope.json").read_text())

    def test_scope_file_has_contract_directories(self):
        self.assertEqual(len(self.scope["wpt"]), 26)
        self.assertEqual(self.scope["wpt"][0], "html/syntax/parsing")
        self.assertEqual(self.scope["wpt"][-1], "css/css-color")
        self.assertEqual(
            self.scope["successors"],
            {"html/syntax/serializing": "html/syntax/serializing-html-fragments"},
        )

    def test_complete_manifest_passes(self):
        manifest = synthetic_manifest(self.scope)
        self.assertEqual(scope_check.scope_findings(manifest, self.scope), [])

    def test_missing_directory_fails(self):
        manifest = synthetic_manifest(self.scope)
        del manifest["items"]["testharness"]["css"]["css-grid"]
        self.assertEqual(scope_check.scope_findings(manifest, self.scope), ["css/css-grid"])

    def test_empty_directory_fails(self):
        manifest = synthetic_manifest(self.scope)
        manifest["items"]["testharness"]["encoding"] = {"sub": {}}
        self.assertEqual(scope_check.scope_findings(manifest, self.scope), ["encoding"])

    def test_successor_counts_for_missing_directory(self):
        scope = dict(self.scope, wpt=["html/syntax/serializing"], wpt_extra=[])
        manifest = synthetic_manifest(self.scope)
        self.assertEqual(scope_check.scope_findings(manifest, scope), [])
        del manifest["items"]["reftest"]["html"]
        self.assertEqual(
            scope_check.scope_findings(manifest, scope), ["html/syntax/serializing"]
        )

    def test_old_manifest_version_fails(self):
        manifest = synthetic_manifest(self.scope)
        manifest["version"] = 7
        self.assertEqual(len(scope_check.scope_findings(manifest, self.scope)), 1)

    def test_main_prints_fail_and_ok(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "MANIFEST.json"
            manifest = synthetic_manifest(self.scope)
            path.write_text(json.dumps(manifest))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(scope_check.main(["--manifest", str(path)]), 0)
            self.assertEqual(out.getvalue(), "OK\n")
            del manifest["items"]["testharness"]["url"]
            path.write_text(json.dumps(manifest))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(scope_check.main(["--manifest", str(path)]), 1)
            self.assertEqual(out.getvalue(), "FAIL url\n")


def fake_binary(path, output):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"#!/bin/sh\necho '{output}'\n")
    path.chmod(0o755)
    return path


class PinsCheckTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        boa = fake_binary(self.root / "bench/cache/boa/boa", "boa 0.22.0")
        self.boa_sha = hashlib.sha256(boa.read_bytes()).hexdigest()
        self.pins = {
            "node": {
                "version": "26.13.0",
                "binary": str(fake_binary(self.root / "bin/node", "v26.13.0")),
                "package": "nodejs 26.13.0-1",
            },
            "xvfb": {
                "version": "21.1.25",
                "binary": str(fake_binary(self.root / "bin/Xvfb", "")),
                "package": "xorg-server-xvfb 21.1.25-2",
            },
            "boa": {
                "version": "0.22.0",
                "url": "https://example.invalid/boa-x86_64-unknown-linux-gnu",
                "sha256": self.boa_sha,
                "binary": "bench/cache/boa/boa",
                "version_output": "boa 0.22.0",
            },
            "data": {"unicode": "17.0.0"},
        }

        self.xvfb_output = "X.Org vendor release 12101025 (1.21.1.25)"
        patcher = mock.patch.object(
            pins_check, "xvfb_version", side_effect=lambda binary: (self.xvfb_output, None)
        )
        patcher.start()
        self.addCleanup(patcher.stop)

    def tearDown(self):
        self._tmp.cleanup()

    def write_pins(self):
        path = self.root / "pins.json"
        path.write_text(json.dumps(self.pins))
        return path

    def run_main(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = pins_check.main(self.root, self.write_pins())
        return code, out.getvalue()

    def test_matching_binaries(self):
        self.assertEqual(pins_check.binary_findings(self.pins, self.root), ([], []))
        self.assertEqual(self.run_main(), (0, "OK\n"))

    def test_xvfb_version_comes_from_the_x11_setup(self):
        self.assertEqual(pins_check.binary_findings(self.pins, self.root), ([], []))
        pins_check.xvfb_version.assert_called_once_with(Path(self.pins["xvfb"]["binary"]))
        self.xvfb_output = "X.Org vendor release 12101024 (1.21.1.24)"
        findings, _ = pins_check.binary_findings(self.pins, self.root)
        self.assertEqual(len(findings), 1)
        self.assertIn("xvfb", findings[0])

    def test_mismatched_version(self):
        fake_binary(self.root / "bin/node", "v26.13.1")
        findings, missing = pins_check.binary_findings(self.pins, self.root)
        self.assertEqual(missing, [])
        self.assertEqual(len(findings), 1)
        self.assertIn("node", findings[0])
        code, out = self.run_main()
        self.assertEqual(code, 1)
        self.assertTrue(out.startswith("FAIL node"))

    def test_longer_version_does_not_match(self):
        self.assertFalse(pins_check.version_matches("21.1.25", "X.Org X Server 1.21.1.250"))
        self.assertFalse(pins_check.version_matches("26.13.0", "v126.13.0"))
        self.assertTrue(pins_check.version_matches("159.0.1", "Mozilla Firefox 159.0.1"))

    def test_download_version_output_mismatch(self):
        boa = fake_binary(self.root / "bench/cache/boa/boa", "boa 0.21.0")
        self.pins["boa"]["sha256"] = hashlib.sha256(boa.read_bytes()).hexdigest()
        findings, _ = pins_check.binary_findings(self.pins, self.root)
        self.assertEqual(len(findings), 1)
        self.assertIn("boa", findings[0])

    def test_download_sha256_mismatch(self):
        self.pins["boa"]["sha256"] = "0" * 64
        findings, _ = pins_check.binary_findings(self.pins, self.root)
        self.assertEqual(len(findings), 1)
        self.assertIn("sha256", findings[0])

    def test_missing_package_binary_exits_77(self):
        (self.root / "bin/node").unlink()
        findings, missing = pins_check.binary_findings(self.pins, self.root)
        self.assertEqual(findings, [])
        self.assertEqual(len(missing), 1)
        self.assertIn("bin/node", missing[0])
        code, out = self.run_main()
        self.assertEqual(code, 77)
        self.assertIn("bin/node", out)

    def test_missing_download_is_a_failure(self):
        (self.root / "bench/cache/boa/boa").unlink()
        self.assertEqual(self.run_main()[0], 1)

    def test_tarball_hash_covers_archive(self):
        entry = {"url": "https://x/servo-x86_64-linux-gnu.tar.gz", "binary": "bench/cache/servo/servoshell"}
        self.assertEqual(
            pins_check.hashed_file(self.root, "servo", entry),
            self.root / "bench/cache/servo/servo-x86_64-linux-gnu.tar.gz",
        )

    def test_tree_at_wrong_commit(self):
        source = make_repo(self.root / "src")
        first = commit(source, "a")
        tree = self.root / "bench/cache/wpt"
        fetch.ensure_tree(tree, source.as_uri(), first)
        pins = {"wpt": {"repo": source.as_uri(), "commit": first}}
        self.assertEqual(pins_check.tree_findings(pins, self.root), [])
        pins["wpt"]["commit"] = "f" * 40
        self.assertEqual(len(pins_check.tree_findings(pins, self.root)), 1)
        pins = {"test262": {"repo": source.as_uri(), "commit": first}}
        self.assertIn("no git tree", pins_check.tree_findings(pins, self.root)[0])


# Minimal X server for xvfb_version: honours -displayfd, listens on
# <socket dir>/X7 and answers the connection setup with a vendor release.
FAKE_XVFB = """\
import os, socket, struct, sys
fd = int(sys.argv[sys.argv.index("-displayfd") + 1])
server = socket.socket(socket.AF_UNIX)
server.bind(os.path.join({socket_dir!r}, "X7"))
server.listen(1)
os.write(fd, b"7\\n")
os.close(fd)
conn, _ = server.accept()
conn.recv(12)
conn.sendall(struct.pack("<BxHHHI", {status}, 11, 0, 0, {release}) + bytes(20))
conn.close()
"""


class XvfbVersionTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self):
        self._tmp.cleanup()

    def fake_xvfb(self, status=1, release=12101025):
        script = self.root / "fake_xvfb.py"
        script.write_text(FAKE_XVFB.format(socket_dir=str(self.root), status=status, release=release))
        binary = self.root / "Xvfb"
        binary.write_text(f"#!/bin/sh\nexec {sys.executable} {script} \"$@\"\n")
        binary.chmod(0o755)
        return binary

    def test_decode_vendor_release(self):
        self.assertEqual(pins_check.decode_vendor_release(12101025), "1.21.1.25")
        self.assertEqual(pins_check.decode_vendor_release(12001020), "1.20.1.20")

    def test_setup_request(self):
        self.assertEqual(pins_check.setup_request(), b"l\x00\x0b\x00\x00\x00" + bytes(6))

    def test_parse_setup_reply(self):
        ok = struct.pack("<BxHHHI", 1, 11, 0, 0, 12101025)
        self.assertEqual(pins_check.parse_setup_reply(ok), (12101025, None))
        self.assertIsNone(pins_check.parse_setup_reply(b"\x00" + ok[1:])[0])
        self.assertIsNone(pins_check.parse_setup_reply(ok[:6])[0])

    def test_fake_server(self):
        output, error = pins_check.xvfb_version(self.fake_xvfb(), 10, self.root)
        self.assertIsNone(error)
        self.assertEqual(output, "X.Org vendor release 12101025 (1.21.1.25)")
        self.assertTrue(pins_check.version_matches("21.1.25", output))

    def test_refused_setup(self):
        output, error = pins_check.xvfb_version(self.fake_xvfb(status=0), 10, self.root)
        self.assertIsNone(output)
        self.assertIn("refused", error)

    def test_binary_without_displayfd(self):
        output, error = pins_check.xvfb_version(fake_binary(self.root / "Xvfb", "usage"), 10, self.root)
        self.assertIsNone(output)
        self.assertIn("display", error)

    @unittest.skipUnless(shutil.which("Xvfb"), "Xvfb not installed")
    def test_installed_xvfb(self):
        output, error = pins_check.xvfb_version(shutil.which("Xvfb"))
        self.assertIsNone(error)
        self.assertRegex(output, r"^X\.Org vendor release \d+ \(\d+\.\d+\.\d+\.\d+\)$")


GIT_ENV = {
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@example.invalid",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@example.invalid",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
}


def run_git(repo, *args):
    env = dict(os.environ, **GIT_ENV)
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, capture_output=True, text=True, env=env
    ).stdout.strip()


def make_repo(path):
    path.mkdir(parents=True)
    run_git(path, "init", "-q")
    run_git(path, "config", "uploadpack.allowAnySHA1InWant", "true")
    return path


def commit(repo, content):
    (repo / "file.txt").write_text(content)
    run_git(repo, "add", "file.txt")
    run_git(repo, "commit", "-q", "-m", content)
    return run_git(repo, "rev-parse", "HEAD")


class FetchIdempotenceTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.env = {k: os.environ.get(k) for k in GIT_ENV}
        os.environ.update(GIT_ENV)

    def tearDown(self):
        for key, value in self.env.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        self._tmp.cleanup()

    def test_ensure_tree_fetches_once(self):
        source = make_repo(self.root / "src")
        first = commit(source, "one")
        second = commit(source, "two")
        tree = self.root / "cache/wpt"

        self.assertTrue(fetch.ensure_tree(tree, source.as_uri(), first))
        self.assertEqual(fetch.tree_head(tree), first)
        self.assertEqual((tree / "file.txt").read_text(), "one")
        # Shallow: only the pinned commit is present.
        self.assertEqual(run_git(tree, "rev-list", "--count", "HEAD"), "1")

        self.assertFalse(fetch.ensure_tree(tree, source.as_uri(), first))
        self.assertEqual(fetch.tree_head(tree), first)

        self.assertTrue(fetch.ensure_tree(tree, source.as_uri(), second))
        self.assertEqual((tree / "file.txt").read_text(), "two")
        self.assertFalse(fetch.ensure_tree(tree, source.as_uri(), second))

    def test_ensure_tree_rejects_unknown_commit(self):
        source = make_repo(self.root / "src")
        commit(source, "one")
        with self.assertRaises(fetch.FetchError):
            fetch.ensure_tree(self.root / "cache/wpt", source.as_uri(), "e" * 40)

    def test_ensure_patch_applies_once(self):
        source = make_repo(self.root / "src")
        first = commit(source, "one\n")
        tree = self.root / "cache/wpt"
        fetch.ensure_tree(tree, source.as_uri(), first)
        patch = self.root / "vidrio.patch"
        self.assertFalse(fetch.ensure_patch(tree, patch))
        (source / "file.txt").write_text("one\ntwo\n")
        patch.write_text(run_git(source, "diff") + "\n")
        self.assertTrue(fetch.ensure_patch(tree, patch))
        self.assertEqual((tree / "file.txt").read_text(), "one\ntwo\n")
        self.assertFalse(fetch.ensure_patch(tree, patch))

    def test_ensure_download_checks_sha256(self):
        payload = self.root / "payload.bin"
        payload.write_bytes(b"boa binary")
        sha = hashlib.sha256(b"boa binary").hexdigest()
        dest = self.root / "cache/boa/boa"
        self.assertTrue(fetch.ensure_download(payload.as_uri(), sha, dest))
        self.assertFalse(fetch.ensure_download(payload.as_uri(), sha, dest))
        other = self.root / "cache/other/boa"
        with self.assertRaises(fetch.FetchError):
            fetch.ensure_download(payload.as_uri(), "0" * 64, other)
        self.assertFalse(other.exists())
        self.assertFalse(other.with_name("boa.part").exists())


class FetchWptEnvTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tree = Path(self._tmp.name) / "wpt"
        python = self.tree / "_venv3/bin/python"
        python.parent.mkdir(parents=True)
        python.write_text("")

    def tearDown(self):
        self._tmp.cleanup()

    def test_requirements_cover_wpt_run(self):
        cmd = fetch.requirements_command("py", self.tree)
        self.assertEqual(cmd[:3], ["py", "-m", "pip"])
        files = [cmd[i + 1] for i, arg in enumerate(cmd) if arg == "-r"]
        self.assertEqual(files, [str(self.tree / r) for r in fetch.WPT_REQUIREMENTS])
        for name in ("requirements_firefox.txt", "requirements_chromium.txt",
                     "requirements_platform_accessibility.txt"):
            self.assertIn(f"tools/wptrunner/{name}", fetch.WPT_REQUIREMENTS)

    def test_servo_link(self):
        servo = Path(self._tmp.name) / "servo"
        servo.mkdir()
        self.assertTrue(fetch.ensure_servo_link(self.tree, servo))
        link = self.tree / "_venv3/servo"
        self.assertEqual(link.resolve(), servo.resolve())
        self.assertFalse(fetch.ensure_servo_link(self.tree, servo))
        link.unlink()
        link.mkdir()
        with self.assertRaises(fetch.FetchError):
            fetch.ensure_servo_link(self.tree, servo)

    def test_downloads_are_skipped_when_present(self):
        dest = Path(self._tmp.name) / "mojojs"
        (fetch.mojojs_gen(dest)).mkdir(parents=True)
        (fetch.mojojs_gen(dest) / "DOWNLOADED_FROM").write_text("u")
        (fetch.openh264_dir(self.tree) / "2.6.0").mkdir(parents=True)
        self.assertEqual(fetch.openh264_dir(self.tree),
                         self.tree / "_venv3/browsers/nightly/gmp-gmpopenh264")
        with mock.patch.object(fetch.subprocess, "run") as run:
            self.assertFalse(fetch.ensure_mojojs(self.tree, "/usr/bin/chromium", dest))
            self.assertFalse(fetch.ensure_openh264(self.tree, "/usr/bin/firefox"))
            run.assert_not_called()


class PublishedListsTest(unittest.TestCase):
    def test_lists_start_empty(self):
        for name in ("excluded.json", "flaky.json"):
            self.assertEqual(json.loads((BENCH / name).read_text()), {"tests": []})

    def test_pins_cover_contract_pieces(self):
        pins = json.loads((BENCH / "pins.json").read_text())
        self.assertEqual(pins["wpt"]["commit"], "77787fe5fe2339a45c740638c5a65345911e80bf")
        self.assertEqual(pins["test262"]["commit"], "9ae074624e2de0ac79e5e29945d2523c69f8582f")
        self.assertEqual(pins["run"]["memory_limit_bytes"], 10 * 1024**3)


if __name__ == "__main__":
    unittest.main()
