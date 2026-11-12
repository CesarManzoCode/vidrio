"""Tests for crates/vjs/gen/unicode.py (the ID_Start/ID_Continue generator)."""

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GEN = ROOT / "crates" / "vjs" / "gen" / "unicode.py"

spec = importlib.util.spec_from_file_location("vjs_unicode_gen", GEN)
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)

SAMPLE = """\
# DerivedCoreProperties-17.0.0.txt
0041..005A    ; ID_Start # L&  [26] LATIN CAPITAL LETTER A..LATIN CAPITAL LETTER Z
0061..007A    ; ID_Start # L&  [26] LATIN SMALL LETTER A..LATIN SMALL LETTER Z
00AA          ; ID_Start # Lo       FEMININE ORDINAL INDICATOR
0030..0039    ; ID_Continue # Nd  [10] DIGIT ZERO..DIGIT NINE
0041..005A    ; ID_Continue # L&  [26] LATIN CAPITAL LETTER A..LATIN CAPITAL LETTER Z
005B          ; ID_Continue # (made up, to test merging)
0061..007A    ; ID_Continue # L&  [26] LATIN SMALL LETTER A..LATIN SMALL LETTER Z
0041..005A    ; Alphabetic # not one of the properties we keep
"""


class GeneratorTest(unittest.TestCase):
    def test_parse_keeps_only_identifier_properties(self):
        tables = gen.parse(SAMPLE)
        self.assertEqual(set(tables), {"ID_Start", "ID_Continue"})
        self.assertEqual(tables["ID_Start"], [(0x41, 0x5A), (0x61, 0x7A), (0xAA, 0xAA)])

    def test_merge_joins_adjacent_and_overlapping_ranges(self):
        self.assertEqual(gen.merge([(5, 6), (1, 2), (3, 4), (10, 12), (11, 11)]), [(1, 6), (10, 12)])
        self.assertEqual(gen.parse(SAMPLE)["ID_Continue"], [(0x30, 0x39), (0x41, 0x5B), (0x61, 0x7A)])

    def test_render_writes_rust_tables(self):
        text = gen.render(gen.parse(SAMPLE))
        self.assertIn("pub const ID_START: &[(u32, u32)] = &[", text)
        self.assertIn("    (0x00AA, 0x00AA),", text)
        self.assertIn("pub const ID_CONTINUE: &[(u32, u32)] = &[", text)
        self.assertIn("Unicode 17.0.0", text)

    def test_committed_output_matches_cached_data(self):
        if not gen.CACHE.is_file():
            self.skipTest("DerivedCoreProperties not downloaded (run crates/vjs/gen/unicode.py)")
        expected = gen.render(gen.parse(gen.CACHE.read_text(encoding="utf-8")))
        self.assertEqual(gen.OUT.read_text(encoding="utf-8"), expected)


if __name__ == "__main__":
    unittest.main()
