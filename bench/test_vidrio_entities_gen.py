"""Tests for crates/vidrio/gen/entities.py (named character references)."""

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GEN = ROOT / "crates" / "vidrio" / "gen" / "entities.py"

spec = importlib.util.spec_from_file_location("vidrio_entities_gen", GEN)
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)

SAMPLE = {
    "&amp;": {"codepoints": [38], "characters": "&"},
    "&amp": {"codepoints": [38], "characters": "&"},
    "&AMP;": {"codepoints": [38], "characters": "&"},
    "&NotEqualTilde;": {"codepoints": [8770, 824], "characters": "≂̸"},
}


class GeneratorTest(unittest.TestCase):
    def test_entries_drop_ampersand_and_sort_by_bytes(self):
        items = gen.entries(SAMPLE)
        self.assertEqual(
            items,
            [
                ("AMP;", 38, 0),
                ("NotEqualTilde;", 0x2242, 0x0338),
                ("amp", 38, 0),
                ("amp;", 38, 0),
            ],
        )

    def test_entries_reject_mismatched_characters(self):
        with self.assertRaises(AssertionError):
            gen.entries({"&x;": {"codepoints": [65], "characters": "B"}})

    def test_render_writes_both_code_points(self):
        text = "\n".join(gen.render(gen.entries(SAMPLE)))
        self.assertIn('    ("NotEqualTilde;", 0x2242, 0x0338),', text)
        self.assertIn('    ("amp", 0x0026, 0x0000),', text)
        self.assertIn("pub const ENTITIES: &[(&str, u32, u32)] = &[", text)


if __name__ == "__main__":
    unittest.main()
