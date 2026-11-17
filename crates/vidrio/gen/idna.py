#!/usr/bin/env python3
"""Generate the UTS #46 tables of crates/vidrio/src/generated/idna.rs.

  python3 crates/vidrio/gen/idna.py

Downloads the Unicode 17.0.0 files IdnaMappingTable.txt,
DerivedBidiClass.txt and DerivedJoiningType.txt to bench/cache/data/
(skipped when already there) and writes idna.rs (vault/CONTRACT.md D-19):

- MAPPING: the IDNA Mapping Table as sorted ranges with a status and, for
  `mapped`, the replacement inside MAPPING_TEXT. The URL Standard runs UTS #46
  with UseSTD3ASCIIRules false and nontransitional processing, so the STD3
  statuses of older tables fold into `valid` and `mapped`, and the transitional
  mapping of `deviation` is dropped.
- BIDI_CLASS: Bidi_Class of every code point (RFC 5893, the CheckBidi step).
- JOINING_TYPE: Joining_Type other than U (RFC 5892 Appendix A, CheckJoiners).

Canonical combining class, General_Category and NFC come from vjs's Unicode
tables. Standard library only.
"""

import sys
import urllib.request
from pathlib import Path

VERSION = "17.0.0"
BASE = f"https://www.unicode.org/Public/{VERSION}/"
ROOT = Path(__file__).resolve().parents[3]
CACHE = ROOT / "bench" / "cache" / "data"
OUT = ROOT / "crates" / "vidrio" / "src" / "generated"
MAX = 0x10FFFF

FILES = {
    "IdnaMappingTable": "idna/IdnaMappingTable.txt",
    "DerivedBidiClass": "ucd/extracted/DerivedBidiClass.txt",
    "DerivedJoiningType": "ucd/extracted/DerivedJoiningType.txt",
}

STATUSES = ["valid", "ignored", "mapped", "deviation", "disallowed"]
FOLDED_STATUSES = {"disallowed_STD3_valid": "valid", "disallowed_STD3_mapped": "mapped"}

BIDI_CLASSES = {
    "Left_To_Right": "L",
    "Right_To_Left": "R",
    "Arabic_Letter": "AL",
    "European_Number": "EN",
    "European_Separator": "ES",
    "European_Terminator": "ET",
    "Arabic_Number": "AN",
    "Common_Separator": "CS",
    "Nonspacing_Mark": "NSM",
    "Boundary_Neutral": "BN",
    "Paragraph_Separator": "B",
    "Segment_Separator": "S",
    "White_Space": "WS",
    "Other_Neutral": "ON",
    "Left_To_Right_Embedding": "LRE",
    "Left_To_Right_Override": "LRO",
    "Right_To_Left_Embedding": "RLE",
    "Right_To_Left_Override": "RLO",
    "Pop_Directional_Format": "PDF",
    "Left_To_Right_Isolate": "LRI",
    "Right_To_Left_Isolate": "RLI",
    "First_Strong_Isolate": "FSI",
    "Pop_Directional_Isolate": "PDI",
}
BIDI_ORDER = list(BIDI_CLASSES.values())

JOINING_TYPES = ["C", "D", "L", "R", "T"]


def fetch(name):
    path = CACHE / f"{name}-{VERSION}.txt"
    if not path.exists():
        CACHE.mkdir(parents=True, exist_ok=True)
        url = BASE + FILES[name]
        print(f"downloading {url}", file=sys.stderr)
        with urllib.request.urlopen(url) as response:
            data = response.read()
        tmp = path.with_suffix(".part")
        tmp.write_bytes(data)
        tmp.replace(path)
    return path.read_text(encoding="utf-8")


def code_range(field):
    field = field.strip()
    if ".." in field:
        first, last = field.split("..")
        return int(first, 16), int(last, 16)
    return int(field, 16), int(field, 16)


def data_lines(text):
    for line in text.split("\n"):
        line = line.split("#", 1)[0].strip()
        if line:
            yield [field.strip() for field in line.split(";")]


def mapping_entries(text):
    """(first, last, status, mapping) of IdnaMappingTable.txt, merged."""
    entries = []
    for fields in data_lines(text):
        first, last = code_range(fields[0])
        status = FOLDED_STATUSES.get(fields[1], fields[1])
        if status not in STATUSES:
            raise SystemExit(f"unknown IDNA status {fields[1]!r}")
        mapping = ""
        if status == "mapped":
            mapping = "".join(chr(int(cp, 16)) for cp in fields[2].split())
        if (
            entries
            and entries[-1][1] + 1 == first
            and entries[-1][2] == status
            and entries[-1][3] == mapping
            and status != "mapped"
        ):
            entries[-1] = (entries[-1][0], last, status, mapping)
        else:
            entries.append((first, last, status, mapping))
    covered = 0
    for first, last, _, _ in entries:
        assert first == covered, f"IdnaMappingTable.txt has a gap at {covered:04X}"
        covered = last + 1
    assert covered == MAX + 1, "IdnaMappingTable.txt does not reach U+10FFFF"
    return entries


def property_ranges(text, aliases=None):
    """Ranges of a derived property file, @missing defaults included."""
    values = {}
    for line in text.split("\n"):
        if line.startswith("# @missing:"):
            body = line[len("# @missing:") :].split("#", 1)[0]
            field, value = [f.strip() for f in body.split(";")]
            first, last = code_range(field)
            value = aliases.get(value, value) if aliases else value
            for cp in range(first, last + 1):
                values[cp] = value
    for fields in data_lines(text):
        first, last = code_range(fields[0])
        for cp in range(first, last + 1):
            values[cp] = fields[1]
    return merge_values(values)


def merge_values(values):
    out = []
    for cp in sorted(values):
        value = values[cp]
        if out and out[-1][1] + 1 == cp and out[-1][2] == value:
            out[-1] = (out[-1][0], cp, value)
        else:
            out.append((cp, cp, value))
    return out


def rust_text(s):
    out = []
    for c in s:
        if c.isascii() and c.isalnum():
            out.append(c)
        else:
            out.append(f"\\u{{{ord(c):X}}}")
    return "".join(out)


def render(mapping, bidi, joining):
    lines = [
        "//! UTS #46 data of Unicode 17.0.0: IDNA Mapping Table, Bidi_Class and Joining_Type.",
        "//! Generated by crates/vidrio/gen/idna.py from IdnaMappingTable.txt,",
        "//! DerivedBidiClass.txt and DerivedJoiningType.txt; do not edit.",
        "",
    ]
    for i, status in enumerate(STATUSES):
        lines.append(f"pub const STATUS_{status.upper()}: u8 = {i};")
    lines.append("")
    text = []
    offsets = []
    position = 0
    for _, _, status, value in mapping:
        encoded = value.encode("utf-8")
        offsets.append((position, len(encoded)))
        if encoded:
            text.append(value)
        position += len(encoded)
    lines.append("/// The replacements of the `mapped` ranges, concatenated.")
    lines.append("pub const MAPPING_TEXT: &str = concat!(")
    chunk = []
    for value in text:
        chunk.append(rust_text(value))
        if sum(len(c) for c in chunk) > 80:
            lines.append(f'    "{"".join(chunk)}",')
            chunk = []
    if chunk:
        lines.append(f'    "{"".join(chunk)}",')
    lines.append(");")
    lines.append("")
    lines.append("/// (first, last, status, byte offset and byte length in MAPPING_TEXT).")
    lines.append("pub const MAPPING: &[(u32, u32, u8, u32, u8)] = &[")
    for (first, last, status, _), (offset, length) in zip(mapping, offsets):
        lines.append(
            f"    (0x{first:04X}, 0x{last:04X}, STATUS_{status.upper()}, {offset}, {length}),"
        )
    lines.append("];")
    lines.append("")
    for i, name in enumerate(BIDI_ORDER):
        lines.append(f"pub const BIDI_{name}: u8 = {i};")
    lines.append("")
    lines.append("/// Bidi_Class of every code point, as sorted ranges.")
    lines.append("pub const BIDI_CLASS: &[(u32, u32, u8)] = &[")
    for first, last, value in bidi:
        lines.append(f"    (0x{first:04X}, 0x{last:04X}, BIDI_{value}),")
    lines.append("];")
    lines.append("")
    for i, name in enumerate(JOINING_TYPES):
        lines.append(f"pub const JOINING_{name}: u8 = {i};")
    lines.append("")
    lines.append("/// Joining_Type other than U (Non_Joining), as sorted ranges.")
    lines.append("pub const JOINING_TYPE: &[(u32, u32, u8)] = &[")
    for first, last, value in joining:
        if value != "U":
            lines.append(f"    (0x{first:04X}, 0x{last:04X}, JOINING_{value}),")
    lines.append("];")
    return lines


def main():
    mapping = mapping_entries(fetch("IdnaMappingTable"))
    bidi = property_ranges(fetch("DerivedBidiClass"), BIDI_CLASSES)
    for _, _, value in bidi:
        if value not in BIDI_ORDER:
            raise SystemExit(f"unknown Bidi_Class {value!r}")
    joining = property_ranges(fetch("DerivedJoiningType"), {"Non_Joining": "U"})
    for _, _, value in joining:
        if value != "U" and value not in JOINING_TYPES:
            raise SystemExit(f"unknown Joining_Type {value!r}")
    OUT.mkdir(parents=True, exist_ok=True)
    path = OUT / "idna.rs"
    path.write_text("\n".join(render(mapping, bidi, joining)) + "\n")
    print(f"wrote {path}", file=sys.stderr)


if __name__ == "__main__":
    main()
