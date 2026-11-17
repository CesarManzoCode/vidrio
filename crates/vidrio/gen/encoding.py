#!/usr/bin/env python3
"""Generate the WHATWG Encoding tables of crates/vidrio/src/generated/.

  python3 crates/vidrio/gen/encoding.py

Downloads `encodings.json`, `indexes.json` and every `index-*.txt` of the
pinned commit of whatwg/encoding (bench/pins.json, vault/CONTRACT.md §5.1)
to bench/cache/data/encoding-<commit>/ (skipped when already there) and
writes (D-19):

- encoding_labels.rs: every label with its encoding, sorted by label.
- encoding_single_byte.rs: the 128 upper code points of each single-byte
  encoding.
- encoding_cjk.rs: index Big5, EUC-KR, gb18030, gb18030 ranges, jis0208,
  jis0212 and ISO-2022-JP katakana.

The `index-*.txt` files are the source; `indexes.json` is read only to check
that both agree. A null index entry is written as 0: no index maps a pointer
to U+0000. Standard library only.
"""

import json
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / "crates" / "vidrio" / "src" / "generated"

# Encoding name of encodings.json -> variant of `crate::encoding::Encoding`.
VARIANTS = {
    "UTF-8": "Utf8",
    "IBM866": "Ibm866",
    "ISO-8859-2": "Iso8859_2",
    "ISO-8859-3": "Iso8859_3",
    "ISO-8859-4": "Iso8859_4",
    "ISO-8859-5": "Iso8859_5",
    "ISO-8859-6": "Iso8859_6",
    "ISO-8859-7": "Iso8859_7",
    "ISO-8859-8": "Iso8859_8",
    "ISO-8859-8-I": "Iso8859_8I",
    "ISO-8859-10": "Iso8859_10",
    "ISO-8859-13": "Iso8859_13",
    "ISO-8859-14": "Iso8859_14",
    "ISO-8859-15": "Iso8859_15",
    "ISO-8859-16": "Iso8859_16",
    "KOI8-R": "Koi8R",
    "KOI8-U": "Koi8U",
    "macintosh": "Macintosh",
    "windows-874": "Windows874",
    "windows-1250": "Windows1250",
    "windows-1251": "Windows1251",
    "windows-1252": "Windows1252",
    "windows-1253": "Windows1253",
    "windows-1254": "Windows1254",
    "windows-1255": "Windows1255",
    "windows-1256": "Windows1256",
    "windows-1257": "Windows1257",
    "windows-1258": "Windows1258",
    "x-mac-cyrillic": "XMacCyrillic",
    "GBK": "Gbk",
    "gb18030": "Gb18030",
    "Big5": "Big5",
    "EUC-JP": "EucJp",
    "ISO-2022-JP": "Iso2022Jp",
    "Shift_JIS": "ShiftJis",
    "EUC-KR": "EucKr",
    "replacement": "Replacement",
    "UTF-16BE": "Utf16Be",
    "UTF-16LE": "Utf16Le",
    "x-user-defined": "XUserDefined",
}

SINGLE_BYTE_HEADING = "Legacy single-byte encodings"

# Multi-byte indexes: index name -> Rust constant and element type.
CJK = [
    ("big5", "BIG5", "u32"),
    ("euc-kr", "EUC_KR", "u16"),
    ("gb18030", "GB18030", "u16"),
    ("jis0208", "JIS0208", "u16"),
    ("jis0212", "JIS0212", "u16"),
    ("iso-2022-jp-katakana", "ISO_2022_JP_KATAKANA", "u16"),
]


def commit():
    pins = json.loads((ROOT / "bench" / "pins.json").read_text())
    return pins["data"]["whatwg_encoding_commit"]


def cache_dir(rev):
    return ROOT / "bench" / "cache" / "data" / f"encoding-{rev}"


def fetch(rev, name):
    path = cache_dir(rev) / name
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        url = f"https://raw.githubusercontent.com/whatwg/encoding/{rev}/{name}"
        print(f"downloading {url}", file=sys.stderr)
        with urllib.request.urlopen(url) as response:
            data = response.read()
        tmp = path.with_suffix(path.suffix + ".part")
        tmp.write_bytes(data)
        tmp.replace(path)
    return path.read_text(encoding="utf-8")


def parse_index(text):
    """Pointer -> code point of an index-*.txt resource (Encoding §5)."""
    entries = {}
    for line in text.split("\n"):
        if not line or line.startswith("#"):
            continue
        fields = line.split("\t")
        pointer = int(fields[0].strip(), 10)
        entries[pointer] = int(fields[1].strip(), 16)
    return entries


def dense(entries, length):
    """The index as a list of `length` code points, 0 for null."""
    out = [0] * length
    for pointer, code_point in entries.items():
        assert code_point != 0, "an index maps a pointer to U+0000"
        out[pointer] = code_point
    return out


def single_byte_index_name(name):
    # ISO-8859-8-I shares the index of ISO-8859-8 (Encoding §9).
    if name == "ISO-8859-8-I":
        return "iso-8859-8"
    return name.lower()


def encodings_of(encodings_json):
    """(name, labels, heading) of each encoding, in specification order."""
    out = []
    for group in encodings_json:
        for encoding in group["encodings"]:
            if encoding["name"] not in VARIANTS:
                raise SystemExit(f"unknown encoding {encoding['name']!r}")
            out.append((encoding["name"], encoding["labels"], group["heading"]))
    return out


def check_against_json(indexes_json, name, values):
    reference = indexes_json[name]
    if name == "gb18030-ranges":
        assert reference == values, f"{name} differs between index-{name}.txt and indexes.json"
        return
    mapped = {p: v for p, v in enumerate(reference) if v is not None}
    assert mapped == {p: v for p, v in enumerate(values) if v}, (
        f"{name} differs between index-{name}.txt and indexes.json"
    )


def hex_rows(values, per_row=12):
    for i in range(0, len(values), per_row):
        yield "    " + " ".join(f"0x{v:X}," for v in values[i : i + per_row])


def header(what, sources, rev):
    return [
        f"//! {what}",
        f"//! Generated by crates/vidrio/gen/encoding.py from {sources} of",
        f"//! whatwg/encoding {rev}; do not edit.",
        "",
    ]


def render_labels(encodings, rev):
    lines = header("Encoding labels (WHATWG Encoding §4.2), sorted for binary search.", "encodings.json", rev)
    lines.append("use crate::encoding::Encoding;")
    lines.append("")
    lines.append("/// Every encoding in specification order.")
    lines.append("pub const ENCODINGS: &[Encoding] = &[")
    for name, _, _ in encodings:
        lines.append(f"    Encoding::{VARIANTS[name]},")
    lines.append("];")
    lines.append("")
    lines.append("/// (label, encoding), sorted by label.")
    lines.append("pub const LABELS: &[(&str, Encoding)] = &[")
    labels = sorted((label, name) for name, labels, _ in encodings for label in labels)
    for label, name in labels:
        lines.append(f'    ("{label}", Encoding::{VARIANTS[name]}),')
    lines.append("];")
    return lines


def render_single_byte(encodings, indexes, rev):
    lines = header("Indexes of the legacy single-byte encodings (WHATWG Encoding §9).", "index-*.txt", rev)
    lines.append("use crate::encoding::Encoding;")
    lines.append("")
    done = set()
    names = [name for name, _, heading in encodings if heading == SINGLE_BYTE_HEADING]
    for name in names:
        index = single_byte_index_name(name)
        if index in done:
            continue
        done.add(index)
        const = index.upper().replace("-", "_")
        lines.append(f"/// index {index}: code point of bytes 0x80..=0xFF, 0 when unmapped.")
        lines.append(f"pub const {const}: [u16; 128] = [")
        lines.extend(hex_rows(indexes[index], 8))
        lines.append("];")
        lines.append("")
    lines.append("/// The index of a single-byte encoding; `None` for any other encoding.")
    lines.append("pub fn index(encoding: Encoding) -> Option<&'static [u16; 128]> {")
    lines.append("    Some(match encoding {")
    for name in names:
        const = single_byte_index_name(name).upper().replace("-", "_")
        lines.append(f"        Encoding::{VARIANTS[name]} => &{const},")
    lines.append("        _ => return None,")
    lines.append("    })")
    lines.append("}")
    return lines


def render_cjk(indexes, ranges, rev):
    lines = header("Indexes of the legacy multi-byte encodings (WHATWG Encoding §5).", "index-*.txt", rev)
    for name, const, ty in CJK:
        values = indexes[name]
        lines.append(f"/// index {name}: code point of each pointer, 0 when null.")
        lines.append(f"pub const {const}: [{ty}; {len(values)}] = [")
        lines.extend(hex_rows(values))
        lines.append("];")
        lines.append("")
    lines.append("/// index gb18030 ranges: (pointer, code point) of each range start.")
    lines.append(f"pub const GB18030_RANGES: [(u32, u32); {len(ranges)}] = [")
    for pointer, code_point in ranges:
        lines.append(f"    ({pointer}, 0x{code_point:04X}),")
    lines.append("];")
    return lines


def main():
    rev = commit()
    encodings = encodings_of(json.loads(fetch(rev, "encodings.json")))
    indexes_json = json.loads(fetch(rev, "indexes.json"))
    indexes = {}
    for name, _, heading in encodings:
        if heading == SINGLE_BYTE_HEADING:
            index = single_byte_index_name(name)
            if index not in indexes:
                indexes[index] = dense(parse_index(fetch(rev, f"index-{index}.txt")), 128)
    for name, _, ty in CJK:
        entries = parse_index(fetch(rev, f"index-{name}.txt"))
        values = dense(entries, max(entries) + 1)
        if ty == "u16":
            assert max(values) <= 0xFFFF, f"index {name} does not fit u16"
        indexes[name] = values
    ranges = sorted(parse_index(fetch(rev, "index-gb18030-ranges.txt")).items())
    for name, values in indexes.items():
        check_against_json(indexes_json, name, values)
    check_against_json(indexes_json, "gb18030-ranges", [list(r) for r in ranges])

    OUT.mkdir(parents=True, exist_ok=True)
    outputs = {
        "encoding_labels.rs": render_labels(encodings, rev),
        "encoding_single_byte.rs": render_single_byte(encodings, indexes, rev),
        "encoding_cjk.rs": render_cjk(indexes, ranges, rev),
    }
    for file, lines in outputs.items():
        (OUT / file).write_text("\n".join(lines) + "\n")
        print(f"wrote {OUT / file}", file=sys.stderr)


if __name__ == "__main__":
    main()
