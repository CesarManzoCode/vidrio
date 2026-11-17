"""Tests for crates/vidrio/gen/encoding.py (the WHATWG Encoding tables)."""

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GEN = ROOT / "crates" / "vidrio" / "gen" / "encoding.py"

spec = importlib.util.spec_from_file_location("vidrio_encoding_gen", GEN)
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)

INDEX = """\
# For details on index index-example.txt see the Encoding Standard
#
# Identifier: 0
# Date: 2024-09-18

     0\t0x0402\tЂ (CYRILLIC CAPITAL LETTER DJE)
     2\t0x201A\t‚ (SINGLE LOW-9 QUOTATION MARK)
"""

ENCODINGS_JSON = [
    {"heading": "The Encoding", "encodings": [{"name": "UTF-8", "labels": ["utf-8", "utf8"]}]},
    {
        "heading": "Legacy single-byte encodings",
        "encodings": [
            {"name": "ISO-8859-8", "labels": ["iso-8859-8"]},
            {"name": "ISO-8859-8-I", "labels": ["iso-8859-8-i", "logical"]},
        ],
    },
]


class GeneratorTest(unittest.TestCase):
    def test_parse_index_skips_comments_and_reads_hex(self):
        self.assertEqual(gen.parse_index(INDEX), {0: 0x0402, 2: 0x201A})

    def test_dense_fills_nulls_with_zero(self):
        self.assertEqual(gen.dense(gen.parse_index(INDEX), 4), [0x0402, 0, 0x201A, 0])

    def test_iso_8859_8_i_shares_the_iso_8859_8_index(self):
        self.assertEqual(gen.single_byte_index_name("ISO-8859-8-I"), "iso-8859-8")
        self.assertEqual(gen.single_byte_index_name("windows-1252"), "windows-1252")

    def test_labels_are_sorted_with_their_variant(self):
        encodings = gen.encodings_of(ENCODINGS_JSON)
        text = "\n".join(gen.render_labels(encodings, "rev"))
        order = [text.index(f'("{label}"') for label in ["iso-8859-8", "iso-8859-8-i", "logical", "utf-8", "utf8"]]
        self.assertEqual(order, sorted(order))
        self.assertIn('("logical", Encoding::Iso8859_8I),', text)

    def test_unknown_encoding_is_rejected(self):
        with self.assertRaises(SystemExit):
            gen.encodings_of([{"heading": "x", "encodings": [{"name": "UTF-7", "labels": []}]}])

    def test_json_cross_check_detects_differences(self):
        gen.check_against_json({"x": [None, 5]}, "x", [0, 5, 0])
        with self.assertRaises(AssertionError):
            gen.check_against_json({"x": [None, 6]}, "x", [0, 5])

    def test_committed_labels_match_cached_data(self):
        rev = gen.commit()
        cache = gen.cache_dir(rev) / "encodings.json"
        if not cache.is_file():
            self.skipTest("encodings.json not downloaded (run crates/vidrio/gen/encoding.py)")
        import json

        expected = "\n".join(gen.render_labels(gen.encodings_of(json.loads(cache.read_text())), rev)) + "\n"
        self.assertEqual((gen.OUT / "encoding_labels.rs").read_text(encoding="utf-8"), expected)


if __name__ == "__main__":
    unittest.main()
