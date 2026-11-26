"""Tests for crates/vidrio/gen/linebreak.py (the UAX #14 tables)."""

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GEN = ROOT / "crates" / "vidrio" / "gen" / "linebreak.py"

spec = importlib.util.spec_from_file_location("vidrio_linebreak_gen", GEN)
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)

LINE_BREAK = """\
# LineBreak.txt
# @missing: 0000..10FFFF; XX
0020           ; SP # Zs         SPACE
0021           ; EX # Po         EXCLAMATION MARK
0041..005A     ; AL # Lu    [26] LATIN CAPITAL LETTER A..LATIN CAPITAL LETTER Z
058A           ; HH # Pd         ARMENIAN HYPHEN
"""

EAST_ASIAN = """\
# @missing: 0000..10FFFF; N
0041..005A     ; Na # Lu    [26] LATIN CAPITAL LETTER A..LATIN CAPITAL LETTER Z
3000           ; F  # Zs         IDEOGRAPHIC SPACE
3001..3003     ; W  # Po     [3] IDEOGRAPHIC COMMA..DITTO MARK
FF61           ; H  # Po         HALFWIDTH IDEOGRAPHIC FULL STOP
"""

EMOJI = """\
00A9          ; Emoji                # E0.6   [1] (©️)       copyright
00A9          ; Extended_Pictographic# E0.6   [1] (©️)       copyright
00AE          ; Extended_Pictographic# E0.6   [1] (®️)       registered
00AF..00B0    ; Extended_Pictographic# made up
"""


class GeneratorTest(unittest.TestCase):
    def test_line_break_ranges_fill_gaps_with_xx(self):
        ranges = gen.property_ranges(LINE_BREAK)
        self.assertEqual(ranges[0], (0x0000, 0x001F, "XX"))
        self.assertIn((0x0020, 0x0020, "SP"), ranges)
        self.assertIn((0x0041, 0x005A, "AL"), ranges)
        self.assertIn((0x058A, 0x058A, "HH"), ranges)
        self.assertEqual(ranges[-1], (0x058B, 0x10FFFF, "XX"))
        for _, _, value in ranges:
            self.assertIn(value, gen.CLASSES)

    def test_wide_ranges_merge_f_w_and_h(self):
        wide = gen.ranges_of(EAST_ASIAN, gen.WIDE)
        self.assertEqual(wide, [(0x3000, 0x3003), (0xFF61, 0xFF61)])

    def test_pictographic_ranges_only_take_the_property(self):
        ranges = gen.binary_ranges(EMOJI, "Extended_Pictographic")
        self.assertEqual(ranges, [(0x00A9, 0x00A9), (0x00AE, 0x00B0)])

    def test_render_names_every_class(self):
        text = "\n".join(gen.render([(0, 0x10FFFF, "XX")], [(0x3000, 0x3000)], []))
        self.assertIn("pub const LB_XX: u8 = 0;", text)
        self.assertIn("pub const LB_ZWJ: u8 = 48;", text)
        self.assertIn("(0x0000, 0x10FFFF, LB_XX),", text)
        self.assertIn("(0x3000, 0x3000),", text)


if __name__ == "__main__":
    unittest.main()
