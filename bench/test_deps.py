import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location("deps", Path(__file__).with_name("deps.py"))
deps = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(deps)

LDD_CLEAN = """\
\tlinux-vdso.so.1 (0x00007ffd5a3f2000)
\tlibxcb.so.1 => /usr/lib/libxcb.so.1 (0x00007f1c2a000000)
\tlibxcb-shm.so.0 => /usr/lib/libxcb-shm.so.0 (0x00007f1c29e00000)
\tlibc.so.6 => /usr/lib/libc.so.6 (0x00007f1c29c00000)
\tlibXau.so.6 => /usr/lib/libXau.so.6 (0x00007f1c29a00000)
\tlibXdmcp.so.6 => /usr/lib/libXdmcp.so.6 (0x00007f1c29800000)
\t/lib64/ld-linux-x86-64.so.2 => /usr/lib64/ld-linux-x86-64.so.2 (0x00007f1c2a200000)
"""

LDD_GCC_S = """\
\tlinux-vdso.so.1 (0x00007ffd5a3f2000)
\tlibgcc_s.so.1 => /usr/lib/libgcc_s.so.1 (0x00007f1c2a000000)
\tlibc.so.6 => /usr/lib/libc.so.6 (0x00007f1c29c00000)
\t/lib64/ld-linux-x86-64.so.2 => /usr/lib64/ld-linux-x86-64.so.2 (0x00007f1c2a200000)
"""

LDD_NOT_FOUND = """\
\tlinux-vdso.so.1 (0x00007ffd5a3f2000)
\tlibxcb-shm.so.0 => not found
\tlibc.so.6 => /usr/lib/libc.so.6 (0x00007f1c29c00000)
"""


class LddTest(unittest.TestCase):
    def test_allowed_libraries_pass(self):
        self.assertEqual(deps.ldd_findings("vidrio", LDD_CLEAN), [])

    def test_basename_of_absolute_loader(self):
        names = [name for name, _ in deps.parse_ldd(LDD_CLEAN)]
        self.assertIn("ld-linux-x86-64.so.2", names)
        self.assertIn("linux-vdso.so.1", names)

    def test_libgcc_s_fails(self):
        findings = deps.ldd_findings("vidrio", LDD_GCC_S)
        self.assertEqual(len(findings), 1)
        self.assertIn("libgcc_s.so.1", findings[0])

    def test_not_found_fails_even_if_allowed(self):
        findings = deps.ldd_findings("vidrio", LDD_NOT_FOUND)
        self.assertEqual(len(findings), 1)
        self.assertIn("libxcb-shm.so.0 not found", findings[0])

    def test_missing_binary_fails(self):
        findings = deps.run_ldd(["/nonexistent/vidrio"])
        self.assertEqual(len(findings), 1)


def _write(root, rel, text):
    path = Path(root) / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


CLEAN_LOCK = """\
version = 4

[[package]]
name = "vgfx"
version = "0.1.0"
dependencies = [
 "vnet",
]

[[package]]
name = "vnet"
version = "0.1.0"
"""


class ForbiddenTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        _write(self.root, "Cargo.toml", '[workspace]\nmembers = ["crates/*"]\n')
        _write(self.root, "Cargo.lock", CLEAN_LOCK)
        _write(
            self.root,
            "crates/vgfx/Cargo.toml",
            '[package]\nname = "vgfx"\n\n[dependencies]\nvnet = { path = "../vnet" }\n',
        )
        _write(
            self.root,
            "crates/vidrio/src/lib.rs",
            "mod selectors;\npub use selectors::matches;\n",
        )
        _write(
            self.root,
            "crates/vidrio/src/selectors.rs",
            "// CSS selectors: our own module, not the crate.\npub fn matches() -> bool { true }\n",
        )

    def tearDown(self):
        self._tmp.cleanup()

    def findings(self):
        return deps.forbidden_findings(self.root)

    def test_clean_tree_and_own_selectors_module_pass(self):
        self.assertEqual(self.findings(), [])

    def test_harfbuzz_dependency_fails(self):
        _write(
            self.root,
            "crates/vgfx/Cargo.toml",
            '[package]\nname = "vgfx"\n\n[dependencies]\nvnet = { path = "../vnet" }\n'
            'HarfBuzz = "0.6"\n',
        )
        findings = self.findings()
        self.assertEqual(len(findings), 1)
        self.assertIn("harfbuzz", findings[0])

    def test_renamed_and_table_dependencies_fail(self):
        _write(
            self.root,
            "crates/vgfx/Cargo.toml",
            '[package]\nname = "vgfx"\n\n[build-dependencies]\n'
            'shaper = { version = "1", package = "harfbuzz-sys" }\n\n'
            "[dev-dependencies.freetype-rs]\nversion = \"0.36\"\n",
        )
        self.assertEqual(len(self.findings()), 2)

    def test_forbidden_name_outside_dependency_sections_passes(self):
        _write(
            self.root,
            "crates/vgfx/Cargo.toml",
            '[package]\nname = "vgfx"\ndescription = "no harfbuzz, no cairo"\n\n'
            "[features]\nselectors = []\n",
        )
        self.assertEqual(self.findings(), [])

    def test_lock_source_line_fails(self):
        _write(
            self.root,
            "Cargo.lock",
            CLEAN_LOCK
            + '\n[[package]]\nname = "itoa"\nversion = "1.0.11"\n'
            'source = "registry+https://github.com/rust-lang/crates.io-index"\n',
        )
        findings = self.findings()
        self.assertEqual(len(findings), 1)
        self.assertIn("third-party", findings[0])

    def test_link_attribute_fails(self):
        _write(
            self.root,
            "crates/vgfx/src/ffi.rs",
            '#[link(name = "cairo")]\nunsafe extern "C" {}\n',
        )
        findings = self.findings()
        self.assertEqual(len(findings), 1)
        self.assertIn("cairo", findings[0])

    def test_extern_crate_and_link_lib_fail(self):
        _write(self.root, "crates/vjs/src/lib.rs", "extern crate alloc;\nextern crate mozjs;\n")
        _write(
            self.root,
            "crates/vnet/build.rs",
            'fn main() { println!("cargo:rustc-link-lib=dylib=ssl"); '
            'println!("cargo::rustc-link-lib=openssl"); }\n',
        )
        findings = self.findings()
        self.assertEqual(len(findings), 2)
        self.assertTrue(any("mozjs" in f for f in findings))
        self.assertTrue(any("openssl" in f for f in findings))

    def test_all_findings_together_skip_selectors_module(self):
        _write(
            self.root,
            "crates/vgfx/Cargo.toml",
            '[package]\nname = "vgfx"\n\n[dependencies]\nharfbuzz = "0.6"\n',
        )
        _write(
            self.root,
            "Cargo.lock",
            CLEAN_LOCK + '\n[[package]]\nname = "itoa"\nversion = "1.0.11"\nsource = "registry+x"\n',
        )
        _write(self.root, "crates/vgfx/src/ffi.rs", '#[link(name = "cairo")]\nunsafe extern "C" {}\n')
        findings = self.findings()
        self.assertEqual(len(findings), 3)
        self.assertFalse(any("selectors" in f for f in findings))

    def test_bench_cache_and_target_are_skipped(self):
        _write(self.root, "bench/cache/wpt/Cargo.toml", '[dependencies]\nservo = "1"\n')
        _write(self.root, "target/debug/build/Cargo.lock", 'source = "registry+x"\n')
        self.assertEqual(self.findings(), [])

    def test_report_prints_fail_lines_or_ok(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(deps.report([]), 0)
        self.assertEqual(out.getvalue(), "OK\n")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(deps.report(["a", "b"]), 1)
        self.assertEqual(out.getvalue(), "FAIL a\nFAIL b\n")


if __name__ == "__main__":
    unittest.main()
