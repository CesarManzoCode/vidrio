"""Tests for crates/vidrio/gen/idna.py (the UTS #46 tables)."""

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GEN = ROOT / "crates" / "vidrio" / "gen" / "idna.py"

spec = importlib.util.spec_from_file_location("vidrio_idna_gen", GEN)
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)


def mapping_sample(last_line):
    return f"""\
# IdnaMappingTable.txt
0000..002C    ; valid      ;      ; NV8    # 1.1  <control-0000>..COMMA
002D..002E    ; valid                      # 1.1  HYPHEN-MINUS..FULL STOP
002F          ; disallowed_STD3_valid      # 1.1  SOLIDUS
0030..0040    ; valid                      # 1.1  DIGIT ZERO..COMMERCIAL AT
0041          ; mapped     ; 0061          # 1.1  LATIN CAPITAL LETTER A
0042          ; mapped     ; 0062          # 1.1  LATIN CAPITAL LETTER B
0043..00DE    ; disallowed                 # made up
00DF          ; deviation  ; 0073 0073     # 1.1  LATIN SMALL LETTER SHARP S
{last_line}
"""


BIDI = """\
# @missing: 0000..10FFFF; Left_To_Right
# @missing: 0590..05FF; Right_To_Left
0030..0039    ; EN # Nd  [10] DIGIT ZERO..DIGIT NINE
05D0..05EA    ; R  # Lo  [27] HEBREW LETTER ALEF..HEBREW LETTER TAV
"""


class GeneratorTest(unittest.TestCase):
    def test_mapping_merges_and_folds_std3_statuses(self):
        entries = gen.mapping_entries(mapping_sample("00E0..10FFFF  ; valid"))
        self.assertEqual(entries[0], (0x0000, 0x0040, "valid", ""))
        self.assertEqual(entries[1], (0x0041, 0x0041, "mapped", "a"))
        self.assertEqual(entries[2], (0x0042, 0x0042, "mapped", "b"))
        self.assertEqual(entries[4], (0x00DF, 0x00DF, "deviation", ""))

    def test_mapping_must_cover_every_code_point(self):
        with self.assertRaises(AssertionError):
            gen.mapping_entries(mapping_sample("00E0..10FFFE  ; valid"))

    def test_property_ranges_apply_missing_defaults_first(self):
        ranges = gen.property_ranges(BIDI, gen.BIDI_CLASSES)
        self.assertIn((0x0030, 0x0039, "EN"), ranges)
        # The explicit Hebrew letters merge with the R default around them.
        self.assertIn((0x0590, 0x05FF, "R"), ranges)
        self.assertEqual(ranges[-1], (0x0600, 0x10FFFF, "L"))

    def test_render_escapes_non_alphanumerics(self):
        self.assertEqual(gen.rust_text("aß\""), "a\\u{DF}\\u{22}")
        mapping = gen.mapping_entries(mapping_sample("00E0..10FFFF  ; valid"))
        text = "\n".join(gen.render(mapping, [(0, 0x10FFFF, "L")], [(0, 0x10FFFF, "U")]))
        self.assertIn("(0x0041, 0x0041, STATUS_MAPPED, 0, 1),", text)
        self.assertIn("(0x0042, 0x0042, STATUS_MAPPED, 1, 1),", text)
        self.assertIn('    "ab",', text)
        self.assertNotIn("JOINING_U", text)


if __name__ == "__main__":
    unittest.main()
