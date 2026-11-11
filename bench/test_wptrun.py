import hashlib
import importlib.util
import re
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


wptrun = _load("wptrun")

WPT_COMMIT = "77787fe5fe2339a45c740638c5a65345911e80bf"
PATCH_FILES = {
    "tools/wptrunner/wptrunner/browsers/vidrio.py",
    "tools/wptrunner/wptrunner/products.py",
    "tools/wpt/browser.py",
    "tools/wpt/run.py",
}


def pins(extra_args=()):
    return {
        "wpt": {"commit": WPT_COMMIT},
        "firefox": {"version": "159.0.1", "binary": "/usr/bin/firefox"},
        "geckodriver": {"version": "0.37.1", "binary": "/usr/bin/geckodriver"},
        "chromium": {"version": "156.0.8078.79", "binary": "/usr/bin/chromium"},
        "chromedriver": {"version": "156.0.8078.79", "binary": "/usr/bin/chromedriver"},
        "servo": {"version": "0.8.0", "binary": "bench/cache/servo/servoshell"},
        "run": {
            "processes": 4,
            "memory_limit_bytes": 10737418240,
            "chromium_extra_args": list(extra_args),
        },
    }


class TempRoot(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self):
        self._tmp.cleanup()

    def common(self, files):
        return [
            "./wpt", "run",
            "--processes", "4",
            "--headless",
            "--no-manifest-update",
            "--manifest", str(self.root / "bench/cache/wpt/MANIFEST.json"),
            "--no-fail-on-unexpected",
            "--log-wptreport", str(files["json"]),
            "--log-raw", str(files["log"]),
        ]


class CommandTest(TempRoot):
    def files(self, product):
        return wptrun.outputs(self.root / "out", "css/css-grid", product)

    def test_firefox(self):
        files = self.files("firefox")
        cmd = wptrun.wpt_command("firefox", "css/css-grid", files, pins(), self.root)
        self.assertEqual(cmd, self.common(files) + [
            "--binary", "/usr/bin/firefox",
            "--webdriver-binary", "/usr/bin/geckodriver",
            "--prefs-root", str(self.root / "bench/cache/firefox-prefs"),
            "--reftest-external",
            "firefox", "css/css-grid",
        ])

    def test_firefox_prefs_root_with_pinned_profiles(self):
        pinned = self.root / "bench/cache/firefox-prefs/profiles/stable/159.0.1"
        pinned.mkdir(parents=True)
        (pinned / "profiles.json").write_text("{}")
        args = wptrun.product_args("firefox", pins(), self.root)
        self.assertEqual(args[args.index("--prefs-root") + 1], str(pinned))

    def test_chromium(self):
        files = self.files("chromium")
        extra = ["--no-sandbox", "--use-gl=swiftshader"]
        cmd = wptrun.wpt_command("chromium", "css/css-grid", files, pins(extra), self.root)
        self.assertEqual(cmd, self.common(files) + [
            "--binary", "/usr/bin/chromium",
            "--webdriver-binary", "/usr/bin/chromedriver",
            "--mojojs-path", str(self.root / "bench/cache/mojojs/mojojs/gen"),
            "--binary-arg", "--no-sandbox",
            "--binary-arg", "--use-gl=swiftshader",
            "chromium", "css/css-grid",
        ])

    def test_servo(self):
        files = self.files("servo")
        cmd = wptrun.wpt_command("servo", "css/css-grid", files, pins(), self.root)
        self.assertEqual(cmd, self.common(files) + [
            "--binary", str(self.root / "bench/cache/servo/servoshell"),
            "servo", "css/css-grid",
        ])
        self.assertNotIn("--log-wptscreenshot", cmd)

    def test_vidrio(self):
        files = self.files("vidrio")
        cmd = wptrun.wpt_command("vidrio", "/css/css-grid/", files, pins(), self.root)
        binary = str(self.root / "target/release/vidrio")
        self.assertEqual(cmd, self.common(files) + [
            "--log-wptscreenshot", str(files["shots"]),
            "--binary", binary,
            "--webdriver-binary", binary,
            "vidrio", "css/css-grid",
        ])

    def test_outputs(self):
        out = self.root / "out"
        self.assertEqual(
            {k: p.name for k, p in wptrun.outputs(out, "html/syntax/parsing", "vidrio").items()},
            {
                "json": "html__syntax__parsing.json",
                "log": "html__syntax__parsing.log",
                "mem": "html__syntax__parsing.mem",
                "shots": "html__syntax__parsing.shots",
            },
        )
        self.assertNotIn("shots", wptrun.outputs(out, "dom", "firefox"))


class KeyTest(TempRoot):
    def test_slug(self):
        self.assertEqual(wptrun.slug("dom"), "dom")
        self.assertEqual(wptrun.slug("html/syntax/parsing"), "html__syntax__parsing")
        self.assertEqual(wptrun.slug("/css/css-grid/"), "css__css-grid")
        self.assertEqual(wptrun.slug("dom/nodes/Node-cloneNode.html"), "dom__nodes__Node-cloneNode.html")

    def test_reference_keys(self):
        self.assertEqual(wptrun.run_key("firefox", pins()), "159.0.1-77787fe5fe23")
        self.assertEqual(wptrun.run_key("chromium", pins()), "156.0.8078.79-77787fe5fe23")
        self.assertEqual(wptrun.run_key("servo", pins(), run=1), "0.8.0-77787fe5fe23")
        self.assertEqual(wptrun.run_key("servo", pins(), run=2), "0.8.0-77787fe5fe23-r2")

    def test_vidrio_key(self):
        binary = self.root / "target/release/vidrio"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"\x7fELF vidrio")
        digest = hashlib.sha256(b"\x7fELF vidrio").hexdigest()[:16]
        self.assertEqual(wptrun.run_key("vidrio", pins(), 1, self.root), f"{digest}-77787fe5fe23")
        self.assertEqual(wptrun.run_key("vidrio", pins(), 2, self.root), f"{digest}-77787fe5fe23-r2")


class CacheTest(TempRoot):
    def test_existing_report_is_skipped(self):
        out = self.root / "out"
        out.mkdir()
        (out / "dom.json").write_text("{}")
        with mock.patch.object(wptrun.subprocess, "run") as run:
            self.assertEqual(wptrun.run_one("firefox", "dom", out, pins(), force=False), "skipped")
            run.assert_not_called()

    def test_force_runs_again(self):
        out = self.root / "out"
        out.mkdir()
        (out / "dom.json").write_text("{}")
        with mock.patch.object(wptrun.subprocess, "run") as run:
            run.return_value.returncode = 1
            self.assertEqual(wptrun.run_one("firefox", "dom", out, pins(), force=True), "failed")
            run.assert_called_once()
        self.assertFalse((out / "dom.json").exists())


class BinaryTest(TempRoot):
    def test_missing_reference_exits_77(self):
        self.assertEqual(wptrun.check_binaries("servo", pins(), self.root), 77)

    def test_present_reference(self):
        servo = self.root / "bench/cache/servo/servoshell"
        servo.parent.mkdir(parents=True)
        servo.write_bytes(b"")
        self.assertIsNone(wptrun.check_binaries("servo", pins(), self.root))

    def test_reference_binaries(self):
        self.assertEqual(
            wptrun.reference_binaries("firefox", pins(), self.root),
            [Path("/usr/bin/firefox"), Path("/usr/bin/geckodriver")],
        )
        self.assertEqual(
            wptrun.reference_binaries("chromium", pins(), self.root),
            [Path("/usr/bin/chromium"), Path("/usr/bin/chromedriver")],
        )

    def test_missing_vidrio_exits_1(self):
        self.assertEqual(wptrun.check_binaries("vidrio", pins(), self.root), 1)

    def test_chromium_needs_mojojs(self):
        p = pins()
        for name in ("chromium", "chromedriver"):
            binary = self.root / name
            binary.write_bytes(b"")
            p[name]["binary"] = str(binary)
        self.assertEqual(wptrun.check_binaries("chromium", p, self.root), 1)
        (self.root / "bench/cache/mojojs/mojojs/gen").mkdir(parents=True)
        self.assertIsNone(wptrun.check_binaries("chromium", p, self.root))


class IsolationTest(unittest.TestCase):
    def test_full_line(self):
        cmd = wptrun.isolated(
            ["--mem", "/r/dom.mem", "--", "./wpt", "run", "firefox", "dom"],
            uid=1000, gid=1001, python="/usr/bin/python3", script="/repo/bench/wptrun.py",
        )
        self.assertEqual(cmd, [
            "systemd-run", "--user", "--scope", "--quiet", "--collect", "--",
            "unshare", "-rn", "sh", "-c",
            "ip link set lo up && exec unshare --map-user=1000 --map-group=1001 "
            "/usr/bin/python3 /repo/bench/wptrun.py --inner --mem /r/dom.mem -- ./wpt run firefox dom",
        ])

    def test_hosts_mount(self):
        cmd = wptrun.isolated(
            ["--mem", "/r/dom.mem"], uid=1000, gid=1001, python="py", script="s.py",
            hosts="/repo/bench/cache/hosts",
        )
        self.assertEqual(cmd[6:9], ["unshare", "-rnm", "sh"])
        self.assertEqual(
            cmd[-1],
            "ip link set lo up && mount --bind /repo/bench/cache/hosts /etc/hosts && "
            "exec unshare --map-user=1000 --map-group=1001 py s.py --inner --mem /r/dom.mem",
        )

    def test_quoting(self):
        cmd = wptrun.isolated(["--mem", "/a b/x.mem"], uid=1, gid=2, python="py", script="s.py")
        self.assertTrue(cmd[-1].endswith("py s.py --inner --mem '/a b/x.mem'"))

    def test_cgroup_path(self):
        text = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/run-r1.scope\n"
        self.assertEqual(
            wptrun.cgroup_path(text),
            "/user.slice/user-1000.slice/user@1000.service/app.slice/run-r1.scope",
        )
        with self.assertRaises(wptrun.RunError):
            wptrun.cgroup_path("1:name=systemd:/x\n")


class HostsTest(TempRoot):
    def test_write_hosts(self):
        system = self.root / "etc-hosts"
        system.write_text("127.0.0.1 localhost")
        wpt = self.root / "wpt"
        wpt.mkdir()
        (wpt / "wpt").write_text("print('127.0.0.1\\tweb-platform.test')\n")
        dest = wptrun.write_hosts(self.root / "cache/hosts", system, wpt)
        self.assertEqual(dest.read_text(), "127.0.0.1 localhost\n127.0.0.1\tweb-platform.test\n")

    def test_write_hosts_fails_without_entries(self):
        wpt = self.root / "wpt"
        wpt.mkdir()
        (wpt / "wpt").write_text("print('nothing')\n")
        with self.assertRaises(wptrun.RunError):
            wptrun.write_hosts(self.root / "hosts", self.root / "missing", wpt)

    def test_only_chromium_gets_the_hosts_mount(self):
        out = self.root / "out"
        for product, mounted in (("chromium", True), ("firefox", False)):
            with mock.patch.object(wptrun, "write_hosts", return_value=Path("/h")) as write, \
                    mock.patch.object(wptrun.subprocess, "run") as run:
                run.return_value.returncode = 1
                self.assertEqual(wptrun.run_one(product, "dom", out, pins(), force=True), "failed")
                self.assertEqual(write.called, mounted)
                self.assertEqual("-rnm" in run.call_args.args[0], mounted)


class NetProbeTest(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(
            wptrun.parse_net_probe("loopback=ok external=blocked\n"),
            {"loopback": "ok", "external": "blocked"},
        )
        self.assertEqual(
            wptrun.parse_net_probe("noise\nloopback=fail external=reachable\n"),
            {"loopback": "fail", "external": "reachable"},
        )

    def test_parse_rejects_other_output(self):
        self.assertIsNone(wptrun.parse_net_probe(""))
        self.assertIsNone(wptrun.parse_net_probe("loopback=yes external=blocked"))
        self.assertIsNone(wptrun.parse_net_probe("external=blocked loopback=ok"))

    def test_probe_line_shape(self):
        with mock.patch.object(wptrun, "probe_loopback", return_value="ok"), \
                mock.patch.object(wptrun, "probe_external", return_value="blocked"):
            self.assertEqual(wptrun.probe_line(), "loopback=ok external=blocked")


class PatchTest(unittest.TestCase):
    def test_touches_exactly_the_four_files(self):
        text = (BENCH / "wpt" / "vidrio.patch").read_text()
        headers = re.findall(r"^diff --git a/(\S+) b/(\S+)$", text, re.M)
        self.assertEqual(len(headers), 4)
        for a, b in headers:
            self.assertEqual(a, b)
        self.assertEqual({a for a, _ in headers}, PATCH_FILES)

    def test_creates_the_product_module(self):
        text = (BENCH / "wpt" / "vidrio.patch").read_text()
        self.assertIn(
            "diff --git a/tools/wptrunner/wptrunner/browsers/vidrio.py "
            "b/tools/wptrunner/wptrunner/browsers/vidrio.py\nnew file mode",
            text,
        )


if __name__ == "__main__":
    unittest.main()
