#!/usr/bin/env python3
"""Generate the Unicode 17.0.0 tables of crates/vjs/src/generated/.

  python3 crates/vjs/gen/unicode.py

Downloads the Unicode Character Database files of Unicode 17.0.0 to
bench/cache/data/ (skipped when already there) and writes (vault/CONTRACT.md
§3.4, D-19):

- unicode_id.rs: ID_Start and ID_Continue for the lexer.
- unicode_case.rs: simple and full (unconditional SpecialCasing) lower,
  upper and title case mappings, simple and full case folding.
- unicode_norm.rs: canonical combining classes, fully expanded canonical
  and compatibility decompositions, and the primary composites.
- unicode_props.rs: every property `\\p{…}` accepts in ECMA-262 RegExp
  (§22.2.2.9): the binary properties, General_Category,
  Script and Script_Extensions values with their aliases, and the
  properties of strings of the `v` flag.

All tables are sorted ranges or sorted keys for binary search. Standard
library only.
"""

import sys
import urllib.request
from pathlib import Path

VERSION = "17.0.0"
BASE = f"https://www.unicode.org/Public/{VERSION}/"
ROOT = Path(__file__).resolve().parents[3]
CACHE = ROOT / "bench" / "cache" / "data"
OUT = ROOT / "crates" / "vjs" / "src" / "generated"
MAX = 0x10FFFF

FILES = {
    "UnicodeData": "ucd/UnicodeData.txt",
    "SpecialCasing": "ucd/SpecialCasing.txt",
    "CaseFolding": "ucd/CaseFolding.txt",
    "DerivedNormalizationProps": "ucd/DerivedNormalizationProps.txt",
    "CompositionExclusions": "ucd/CompositionExclusions.txt",
    "PropList": "ucd/PropList.txt",
    "DerivedCoreProperties": "ucd/DerivedCoreProperties.txt",
    "Scripts": "ucd/Scripts.txt",
    "ScriptExtensions": "ucd/ScriptExtensions.txt",
    "PropertyValueAliases": "ucd/PropertyValueAliases.txt",
    "PropertyAliases": "ucd/PropertyAliases.txt",
    "emoji-data": "ucd/emoji/emoji-data.txt",
    "emoji-sequences": "emoji/emoji-sequences.txt",
    "emoji-zwj-sequences": "emoji/emoji-zwj-sequences.txt",
}

# The binary Unicode properties of ECMA-262 §22.2.2.9: canonical name and
# alias. ASCII, Any and Assigned are not in the UCD files and are derived.
BINARY_PROPERTIES = [
    ("ASCII", None),
    ("ASCII_Hex_Digit", "AHex"),
    ("Alphabetic", "Alpha"),
    ("Any", None),
    ("Assigned", None),
    ("Bidi_Control", "Bidi_C"),
    ("Bidi_Mirrored", "Bidi_M"),
    ("Case_Ignorable", "CI"),
    ("Cased", None),
    ("Changes_When_Casefolded", "CWCF"),
    ("Changes_When_Casemapped", "CWCM"),
    ("Changes_When_Lowercased", "CWL"),
    ("Changes_When_NFKC_Casefolded", "CWKCF"),
    ("Changes_When_Titlecased", "CWT"),
    ("Changes_When_Uppercased", "CWU"),
    ("Dash", None),
    ("Default_Ignorable_Code_Point", "DI"),
    ("Deprecated", "Dep"),
    ("Diacritic", "Dia"),
    ("Emoji", None),
    ("Emoji_Component", "EComp"),
    ("Emoji_Modifier", "EMod"),
    ("Emoji_Modifier_Base", "EBase"),
    ("Emoji_Presentation", "EPres"),
    ("Extended_Pictographic", "ExtPict"),
    ("Extender", "Ext"),
    ("Grapheme_Base", "Gr_Base"),
    ("Grapheme_Extend", "Gr_Ext"),
    ("Hex_Digit", "Hex"),
    ("IDS_Binary_Operator", "IDSB"),
    ("IDS_Trinary_Operator", "IDST"),
    ("ID_Continue", "IDC"),
    ("ID_Start", "IDS"),
    ("Ideographic", "Ideo"),
    ("Join_Control", "Join_C"),
    ("Logical_Order_Exception", "LOE"),
    ("Lowercase", "Lower"),
    ("Math", None),
    ("Noncharacter_Code_Point", "NChar"),
    ("Pattern_Syntax", "Pat_Syn"),
    ("Pattern_White_Space", "Pat_WS"),
    ("Quotation_Mark", "QMark"),
    ("Radical", None),
    ("Regional_Indicator", "RI"),
    ("Sentence_Terminal", "STerm"),
    ("Soft_Dotted", "SD"),
    ("Terminal_Punctuation", "Term"),
    ("Unified_Ideograph", "UIdeo"),
    ("Uppercase", "Upper"),
    ("Variation_Selector", "VS"),
    ("White_Space", "space"),
    ("XID_Continue", "XIDC"),
    ("XID_Start", "XIDS"),
]

# The binary Unicode properties of strings of ECMA-262 §22.2.2.9.
STRING_PROPERTIES = [
    "Basic_Emoji",
    "Emoji_Keycap_Sequence",
    "RGI_Emoji_Modifier_Sequence",
    "RGI_Emoji_Flag_Sequence",
    "RGI_Emoji_Tag_Sequence",
    "RGI_Emoji_ZWJ_Sequence",
    "RGI_Emoji",
]

GC_GROUPS = {
    "C": ["Cc", "Cf", "Cn", "Co", "Cs"],
    "L": ["Ll", "Lm", "Lo", "Lt", "Lu"],
    "LC": ["Ll", "Lt", "Lu"],
    "M": ["Mc", "Me", "Mn"],
    "N": ["Nd", "Nl", "No"],
    "P": ["Pc", "Pd", "Pe", "Pf", "Pi", "Po", "Ps"],
    "S": ["Sc", "Sk", "Sm", "So"],
    "Z": ["Zl", "Zp", "Zs"],
}


# ----- reading -----


def fetch(name):
    path = CACHE / f"{name}-{VERSION}.txt"
    if path.is_file():
        return path.read_text(encoding="utf-8")
    url = BASE + FILES[name]
    CACHE.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read().decode("utf-8")
    # UnicodeData.txt has no header; the others name their version.
    if name != "UnicodeData":
        head = "\n".join(data.splitlines()[:12])
        if VERSION not in head and f"Version: {VERSION[:4]}" not in head:
            sys.exit(f"unexpected header in {url}")
    path.write_text(data, encoding="utf-8")
    return data


def records(text):
    """Yields the `;`-separated fields of each data line, comments removed."""
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            yield [field.strip() for field in line.split(";")]


def code_range(field):
    if ".." in field:
        first, last = (int(part, 16) for part in field.split(".."))
    else:
        first = last = int(field, 16)
    return first, last


def code_points(field):
    return [int(part, 16) for part in field.split()]


def merge(ranges):
    merged = []
    for first, last in sorted(ranges):
        if merged and first <= merged[-1][1] + 1:
            merged[-1] = (merged[-1][0], max(merged[-1][1], last))
        else:
            merged.append((first, last))
    return merged


def to_ranges(points):
    return merge((p, p) for p in points)


def complement(ranges):
    out = []
    next_start = 0
    for first, last in merge(ranges):
        if first > next_start:
            out.append((next_start, first - 1))
        next_start = last + 1
    if next_start <= MAX:
        out.append((next_start, MAX))
    return out


def property_file(text):
    """{property: [(first, last), ...]} of a binary property file."""
    found = {}
    for fields in records(text):
        if len(fields) == 2:
            found.setdefault(fields[1], []).append(code_range(fields[0]))
    return {name: merge(r) for name, r in found.items()}


class UnicodeData:
    """The fields of UnicodeData.txt the tables need, with the First/Last
    ranges expanded."""

    def __init__(self, text):
        self.gc = {}
        self.ccc = {}
        self.decomposition = {}
        self.upper = {}
        self.lower = {}
        self.title = {}
        self.mirrored = []
        first = None
        for line in text.splitlines():
            if not line:
                continue
            f = line.split(";")
            cp = int(f[0], 16)
            if f[1].endswith(", First>"):
                first = cp
                continue
            span = range(first, cp + 1) if f[1].endswith(", Last>") else range(cp, cp + 1)
            first = None
            for c in span:
                self.gc[c] = f[2]
                if f[3] != "0":
                    self.ccc[c] = int(f[3])
                if f[9] == "Y":
                    self.mirrored.append(c)
            if f[5]:
                parts = f[5].split()
                compat = parts[0].startswith("<")
                if compat:
                    parts = parts[1:]
                self.decomposition[cp] = (compat, [int(p, 16) for p in parts])
            if f[12]:
                self.upper[cp] = int(f[12], 16)
            if f[13]:
                self.lower[cp] = int(f[13], 16)
            if f[14]:
                self.title[cp] = int(f[14], 16)


# ----- case -----


def special_casing(text):
    """Unconditional mappings of SpecialCasing.txt: {cp: (lower, title, upper)}.
    Conditional (language or context) mappings are left to the code: only
    Final_Sigma is language-insensitive."""
    out = {}
    for fields in records(text):
        condition = fields[4] if len(fields) > 4 else ""
        if condition:
            continue
        cp = int(fields[0], 16)
        out[cp] = tuple(code_points(fields[i]) for i in (1, 2, 3))
    return out


def case_folding(text):
    simple, full = {}, {}
    for fields in records(text):
        cp, status, mapping = int(fields[0], 16), fields[1], code_points(fields[2])
        if status in ("C", "S"):
            simple[cp] = mapping[0]
        if status in ("C", "F"):
            full[cp] = mapping
    # Full folding: F entries, else the common (C) ones.
    full_only = {cp: m for cp, m in full.items() if len(m) > 1}
    return simple, full_only


# ----- normalization -----


def expand(cp, data, compat):
    entry = data.decomposition.get(cp)
    if entry is None or (entry[0] and not compat):
        return [cp]
    out = []
    for c in entry[1]:
        out.extend(expand(c, data, compat))
    return out


def normalization(data, derived_norm, exclusions_text):
    canonical, compatibility = {}, {}
    for cp in data.decomposition:
        nfd = expand(cp, data, False)
        if nfd != [cp]:
            canonical[cp] = nfd
        nfkd = expand(cp, data, True)
        if nfkd != [cp]:
            compatibility[cp] = nfkd
    exclusions = set()
    for first, last in derived_norm.get("Full_Composition_Exclusion", []):
        exclusions.update(range(first, last + 1))
    for fields in records(exclusions_text):
        first, last = code_range(fields[0])
        for cp in range(first, last + 1):
            if cp not in exclusions:
                sys.exit(f"CompositionExclusions U+{cp:04X} not in Full_Composition_Exclusion")
    compositions = []
    for cp, (compat, parts) in data.decomposition.items():
        if compat or len(parts) != 2 or cp in exclusions:
            continue
        compositions.append((parts[0], parts[1], cp))
    compositions.sort()
    ccc = []
    for cp in sorted(data.ccc):
        value = data.ccc[cp]
        if ccc and ccc[-1][1] == cp - 1 and ccc[-1][2] == value:
            ccc[-1] = (ccc[-1][0], cp, value)
        else:
            ccc.append((cp, cp, value))
    return canonical, compatibility, compositions, ccc


# ----- properties -----


def derived_norm_file(text):
    """Binary properties of DerivedNormalizationProps.txt (two fields)."""
    found = {}
    for fields in records(text):
        if len(fields) == 2:
            found.setdefault(fields[1], []).append(code_range(fields[0]))
    return {name: merge(r) for name, r in found.items()}


def value_aliases(text, prop):
    """[[canonical long name, short name, other aliases...]] of `prop`."""
    out = []
    for fields in records(text):
        if fields[0] != prop:
            continue
        short, long = fields[1], fields[2]
        # The short name stays second even when it equals the long one.
        names = [long, short]
        for n in fields[3:]:
            if n and n not in names:
                names.append(n)
        out.append(names)
    return out


def property_aliases(text):
    out = {}
    for fields in records(text):
        out[fields[1]] = fields[0]
    return out


def general_categories(data):
    by_value = {}
    for cp, gc in data.gc.items():
        by_value.setdefault(gc, []).append(cp)
    tables = {gc: to_ranges(points) for gc, points in by_value.items()}
    assigned = merge(r for ranges in tables.values() for r in ranges)
    tables["Cn"] = complement(assigned)
    for group, members in GC_GROUPS.items():
        tables[group] = merge(r for m in members for r in tables.get(m, []))
    # The map from code point to category, for general_category(cp).
    gc_map = []
    for cp in sorted(data.gc):
        value = data.gc[cp]
        if gc_map and gc_map[-1][1] == cp - 1 and gc_map[-1][2] == value:
            gc_map[-1] = (gc_map[-1][0], cp, value)
        else:
            gc_map.append((cp, cp, value))
    return tables, assigned, gc_map


def scripts(scripts_text, extensions_text, aliases):
    short_to_long = {names[1]: names[0] for names in aliases}
    by_script = {}
    for fields in records(scripts_text):
        by_script.setdefault(fields[1], []).append(code_range(fields[0]))
    tables = {name: merge(r) for name, r in by_script.items()}
    listed = merge(r for ranges in tables.values() for r in ranges)
    tables["Unknown"] = complement(listed)
    # Script_Extensions: the listed sets, otherwise {Script}.
    explicit = {}
    for fields in records(extensions_text):
        first, last = code_range(fields[0])
        longs = [short_to_long[s] for s in fields[1].split()]
        for cp in range(first, last + 1):
            explicit[cp] = longs
    ext = {name: [] for name in tables}
    overridden = to_ranges(explicit)
    for name, ranges in tables.items():
        ext[name] = subtract(ranges, overridden)
    extra = {}
    for cp, longs in explicit.items():
        for name in longs:
            extra.setdefault(name, []).append(cp)
    for name, points in extra.items():
        ext[name] = merge(ext.get(name, []) + to_ranges(points))
    for names in aliases:
        tables.setdefault(names[0], [])
        ext.setdefault(names[0], [])
    script_map = []
    for name, ranges in tables.items():
        if name == "Unknown":
            continue
        script_map.extend((first, last, name) for first, last in ranges)
    script_map.sort()
    ext_map = []
    for cp in sorted(explicit):
        value = tuple(sorted(explicit[cp]))
        if ext_map and ext_map[-1][1] == cp - 1 and ext_map[-1][2] == value:
            ext_map[-1] = (ext_map[-1][0], cp, value)
        else:
            ext_map.append((cp, cp, value))
    return tables, ext, script_map, ext_map


def subtract(ranges, remove):
    points = set()
    for first, last in ranges:
        points.update(range(first, last + 1))
    for first, last in remove:
        points.difference_update(range(first, last + 1))
    return to_ranges(points)


def string_properties(sequences_text, zwj_text):
    singles = {name: [] for name in STRING_PROPERTIES}
    strings = {name: [] for name in STRING_PROPERTIES}
    for text in (sequences_text, zwj_text):
        for fields in records(text):
            name = fields[1]
            if name not in singles:
                sys.exit(f"unknown emoji sequence property {name}")
            if ".." in fields[0]:
                singles[name].append(code_range(fields[0]))
                continue
            points = code_points(fields[0])
            if len(points) == 1:
                singles[name].append((points[0], points[0]))
            else:
                strings[name].append(tuple(points))
    for name in STRING_PROPERTIES[:-1]:
        singles["RGI_Emoji"].extend(singles[name])
        strings["RGI_Emoji"].extend(strings[name])
    return (
        {name: merge(r) for name, r in singles.items()},
        {name: sorted(set(s)) for name, s in strings.items()},
    )


# ----- rendering -----


def header(what, sources):
    return [
        f"//! {what} of Unicode {VERSION}.",
        f"//! Generated by crates/vjs/gen/unicode.py from {sources}; do not edit.",
        "",
    ]


def hexes(points):
    return ", ".join(f"0x{p:04X}" for p in points)


def render_ranges(const, ranges, lines):
    lines.append(f"pub const {const}: &[(u32, u32)] = &[")
    for first, last in ranges:
        lines.append(f"    (0x{first:04X}, 0x{last:04X}),")
    lines.append("];")
    lines.append("")


def render_map(const, mapping, lines):
    lines.append(f"pub const {const}: &[(u32, u32)] = &[")
    for cp in sorted(mapping):
        lines.append(f"    (0x{cp:04X}, 0x{mapping[cp]:04X}),")
    lines.append("];")
    lines.append("")


def render_multi(const, mapping, lines):
    lines.append(f"pub const {const}: &[(u32, &[u32])] = &[")
    for cp in sorted(mapping):
        lines.append(f"    (0x{cp:04X}, &[{hexes(mapping[cp])}]),")
    lines.append("];")
    lines.append("")


def write(name, lines):
    path = OUT / name
    path.write_text("\n".join(lines), encoding="utf-8")
    print(f"wrote {path.relative_to(ROOT)}")


def id_tables(text):
    """{ID_Start, ID_Continue: merged ranges} of DerivedCoreProperties.txt."""
    derived = property_file(text)
    return {name: derived.get(name, []) for name in ("ID_Start", "ID_Continue")}


def render_id(tables):
    """The lines of unicode_id.rs."""
    lines = header(
        "ID_Start and ID_Continue", "DerivedCoreProperties.txt"
    )
    render_ranges("ID_START", tables["ID_Start"], lines)
    render_ranges("ID_CONTINUE", tables["ID_Continue"], lines)
    return lines


def render_case(data, special, fold_simple, fold_full):
    lines = header(
        "Case mappings and case folding",
        "UnicodeData.txt, SpecialCasing.txt and CaseFolding.txt",
    )
    lines.append("// Simple mappings (UnicodeData.txt fields 12–14).")
    render_map("SIMPLE_UPPER", data.upper, lines)
    render_map("SIMPLE_LOWER", data.lower, lines)
    render_map("SIMPLE_TITLE", data.title, lines)
    lines.append("// Unconditional full mappings of SpecialCasing.txt that differ from the")
    lines.append("// simple ones; the rest of the full mapping is the simple mapping.")
    for index, const in ((0, "SPECIAL_LOWER"), (1, "SPECIAL_TITLE"), (2, "SPECIAL_UPPER")):
        simple = (data.lower, data.title, data.upper)[index]
        mapping = {}
        for cp, maps in special.items():
            m = maps[index]
            if m != [simple.get(cp, cp)]:
                mapping[cp] = m
        render_multi(const, mapping, lines)
    lines.append("// Simple case folding (status C and S).")
    render_map("FOLD_SIMPLE", fold_simple, lines)
    lines.append("// Full case folding entries longer than one code point (status F); the")
    lines.append("// rest of the full folding is FOLD_SIMPLE restricted to status C.")
    render_multi("FOLD_FULL", fold_full, lines)
    write("unicode_case.rs", lines)


def render_norm(canonical, compatibility, compositions, ccc):
    lines = header(
        "Normalization data",
        "UnicodeData.txt, DerivedNormalizationProps.txt and CompositionExclusions.txt",
    )
    lines.append("/// Non-zero Canonical_Combining_Class values as (first, last, class).")
    lines.append("pub const COMBINING_CLASS: &[(u32, u32, u8)] = &[")
    for first, last, value in ccc:
        lines.append(f"    (0x{first:04X}, 0x{last:04X}, {value}),")
    lines.append("];")
    lines.append("")
    lines.append("/// Full canonical decompositions (NFD of one code point), Hangul excluded.")
    render_multi("CANONICAL_DECOMPOSITION", canonical, lines)
    lines.append("/// Full compatibility decompositions (NFKD of one code point), Hangul excluded.")
    render_multi("COMPATIBILITY_DECOMPOSITION", compatibility, lines)
    lines.append("/// Primary composites as (first, second, composite), sorted by the pair;")
    lines.append("/// Full_Composition_Exclusion and Hangul excluded.")
    lines.append("pub const COMPOSITION: &[(u32, u32, u32)] = &[")
    for a, b, c in compositions:
        lines.append(f"    (0x{a:04X}, 0x{b:04X}, 0x{c:04X}),")
    lines.append("];")
    lines.append("")
    write("unicode_norm.rs", lines)


def str_list(names):
    return "&[" + ", ".join(f'"{n}"' for n in names) + "]"


def render_props(binary, gc_aliases, gc_tables, gc_map, sc_aliases, sc_tables, scx_tables,
                 script_map, ext_map, singles, strings):
    lines = header(
        "Properties of `\\p{…}`",
        "UnicodeData.txt, PropList.txt, DerivedCoreProperties.txt, "
        "DerivedNormalizationProps.txt, Scripts.txt, ScriptExtensions.txt, "
        "PropertyAliases.txt, PropertyValueAliases.txt and the emoji files",
    )
    # Binary properties.
    for name, _ in BINARY_PROPERTIES:
        if name in ("ID_Start", "ID_Continue"):
            continue
        render_ranges(f"BP_{name.upper()}", binary[name], lines)
    lines.append("/// Binary properties of ECMA-262 §22.2.2.9: (name, alias, ranges).")
    lines.append("pub const BINARY_PROPERTIES: &[(&str, &str, &[(u32, u32)])] = &[")
    for name, alias in BINARY_PROPERTIES:
        if name == "ID_Start":
            table = "super::unicode_id::ID_START"
        elif name == "ID_Continue":
            table = "super::unicode_id::ID_CONTINUE"
        else:
            table = f"BP_{name.upper()}"
        lines.append(f'    ("{name}", "{alias or ""}", {table}),')
    lines.append("];")
    lines.append("")
    # General_Category.
    for names in gc_aliases:
        render_ranges(f"GC_{names[1].upper()}", gc_tables[names[1]], lines)
    lines.append("/// General_Category values: (names, canonical first, short second; ranges).")
    lines.append("pub const GENERAL_CATEGORY_VALUES: &[(&[&str], &[(u32, u32)])] = &[")
    for names in gc_aliases:
        lines.append(f"    ({str_list(names)}, GC_{names[1].upper()}),")
    lines.append("];")
    lines.append("")
    lines.append("/// General_Category of the assigned code points as (first, last, short name).")
    lines.append("pub const GENERAL_CATEGORY: &[(u32, u32, &str)] = &[")
    for first, last, value in gc_map:
        lines.append(f'    (0x{first:04X}, 0x{last:04X}, "{value}"),')
    lines.append("];")
    lines.append("")
    # Script and Script_Extensions.
    for names in sc_aliases:
        const = names[1].upper()
        render_ranges(f"SC_{const}", sc_tables[names[0]], lines)
        render_ranges(f"SCX_{const}", scx_tables[names[0]], lines)
    lines.append("/// Script values: (names, canonical first, short second; Script ranges;")
    lines.append("/// Script_Extensions ranges).")
    lines.append(
        "pub const SCRIPT_VALUES: &[(&[&str], &[(u32, u32)], &[(u32, u32)])] = &["
    )
    for names in sc_aliases:
        const = names[1].upper()
        lines.append(f"    ({str_list(names)}, SC_{const}, SCX_{const}),")
    lines.append("];")
    lines.append("")
    lines.append("/// Script of the code points that have one, as (first, last, long name);")
    lines.append("/// the rest are Unknown.")
    lines.append("pub const SCRIPT: &[(u32, u32, &str)] = &[")
    for first, last, value in script_map:
        lines.append(f'    (0x{first:04X}, 0x{last:04X}, "{value}"),')
    lines.append("];")
    lines.append("")
    lines.append("/// Explicit Script_Extensions as (first, last, long names); elsewhere the")
    lines.append("/// extensions are {Script}.")
    lines.append("pub const SCRIPT_EXTENSIONS: &[(u32, u32, &[&str])] = &[")
    for first, last, value in ext_map:
        lines.append(f"    (0x{first:04X}, 0x{last:04X}, {str_list(value)}),")
    lines.append("];")
    lines.append("")
    # Properties of strings.
    for name in STRING_PROPERTIES:
        const = name.upper()
        render_ranges(f"STR_{const}_CODE_POINTS", singles[name], lines)
        lines.append(f"pub const STR_{const}_STRINGS: &[&[u32]] = &[")
        for s in strings[name]:
            lines.append(f"    &[{hexes(s)}],")
        lines.append("];")
        lines.append("")
    lines.append("/// Properties of strings of ECMA-262 §22.2.2.9: (name, single code points,")
    lines.append("/// sequences of two or more code points, sorted).")
    lines.append("pub const STRING_PROPERTIES: &[(&str, &[(u32, u32)], &[&[u32]])] = &[")
    for name in STRING_PROPERTIES:
        const = name.upper()
        lines.append(f'    ("{name}", STR_{const}_CODE_POINTS, STR_{const}_STRINGS),')
    lines.append("];")
    lines.append("")
    write("unicode_props.rs", lines)


def main():
    text = {name: fetch(name) for name in FILES}
    data = UnicodeData(text["UnicodeData"])

    derived = property_file(text["DerivedCoreProperties"])
    ids = id_tables(text["DerivedCoreProperties"])
    for name, ranges in ids.items():
        if not ranges:
            sys.exit(f"no ranges found for {name}")
    write("unicode_id.rs", render_id(ids))

    special = special_casing(text["SpecialCasing"])
    fold_simple, fold_full = case_folding(text["CaseFolding"])
    render_case(data, special, fold_simple, fold_full)

    derived_norm = derived_norm_file(text["DerivedNormalizationProps"])
    canonical, compatibility, compositions, ccc = normalization(
        data, derived_norm, text["CompositionExclusions"]
    )
    render_norm(canonical, compatibility, compositions, ccc)

    gc_tables, assigned, gc_map = general_categories(data)
    binary = {}
    for source in (
        property_file(text["PropList"]),
        derived,
        derived_norm,
        property_file(text["emoji-data"]),
    ):
        for name, ranges in source.items():
            binary.setdefault(name, ranges)
    binary["ASCII"] = [(0, 0x7F)]
    binary["Any"] = [(0, MAX)]
    binary["Assigned"] = assigned
    binary["Bidi_Mirrored"] = to_ranges(data.mirrored)
    aliases = property_aliases(text["PropertyAliases"])
    for name, alias in BINARY_PROPERTIES:
        if name not in binary:
            sys.exit(f"no data for binary property {name}")
        if alias and aliases.get(name) != alias and alias != "space":
            sys.exit(f"alias {alias} of {name} not in PropertyAliases.txt")

    pva = text["PropertyValueAliases"]
    gc_aliases = value_aliases(pva, "gc")
    sc_aliases = value_aliases(pva, "sc")
    for names in gc_aliases:
        if names[1] not in gc_tables:
            sys.exit(f"no table for General_Category {names[1]}")
    sc_tables, scx_tables, script_map, ext_map = scripts(
        text["Scripts"], text["ScriptExtensions"], sc_aliases
    )
    singles, strings = string_properties(text["emoji-sequences"], text["emoji-zwj-sequences"])
    render_props(binary, gc_aliases, gc_tables, gc_map, sc_aliases, sc_tables, scx_tables,
                 script_map, ext_map, singles, strings)


if __name__ == "__main__":
    main()
