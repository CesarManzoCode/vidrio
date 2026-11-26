"""Tests for crates/vidrio/gen/bidi.py (the UAX #9 bracket and mirroring tables)."""

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GEN = ROOT / "crates" / "vidrio" / "gen" / "bidi.py"

spec = importlib.util.spec_from_file_location("vidrio_bidi_gen", GEN)
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)

BRACKETS = """\
# BidiBrackets.txt
005B; 005D; o # LEFT SQUARE BRACKET
0028; 0029; o # LEFT PARENTHESIS
0029; 0028; c # RIGHT PARENTHESIS
"""

MIRRORING = """\
003E; 003C # GREATER-THAN SIGN
003C; 003E # LESS-THAN SIGN
"""


class GeneratorTest(unittest.TestCase):
    def test_brackets_are_sorted_with_their_type(self):
        self.assertEqual(
            gen.brackets(BRACKETS),
            [(0x28, 0x29, True), (0x29, 0x28, False), (0x5B, 0x5D, True)],
        )

    def test_unknown_bracket_type_fails(self):
        with self.assertRaises(SystemExit):
            gen.brackets("0028; 0029; n # LEFT PARENTHESIS\n")

    def test_mirroring_is_sorted(self):
        self.assertEqual(gen.mirroring(MIRRORING), [(0x3C, 0x3E), (0x3E, 0x3C)])

    def test_render(self):
        text = "\n".join(gen.render([(0x28, 0x29, True)], [(0x3C, 0x3E)]))
        self.assertIn("(0x0028, 0x0029, true),", text)
        self.assertIn("(0x003C, 0x003E),", text)


if __name__ == "__main__":
    unittest.main()
