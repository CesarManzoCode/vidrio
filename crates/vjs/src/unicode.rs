//! Unicode 17.0.0 for the engine and the browser: code point classes of
//! the lexical grammar (ECMA-262 §12: identifiers, white space and line
//! terminators), full case mapping and case folding, the normalization
//! forms, and the properties `\p{…}` accepts in RegExp (§22.2.2.9).
//!
//! The data lives in the tables of [`crate::generated`], made by
//! `crates/vjs/gen/unicode.py`. Code points are `u32` so that the lone
//! surrogates of UTF-16 strings pass through unchanged (they have no
//! mapping, no decomposition and combining class 0).

use crate::generated::unicode_case::{
    FOLD_FULL, FOLD_SIMPLE, SIMPLE_LOWER, SIMPLE_TITLE, SIMPLE_UPPER, SPECIAL_LOWER, SPECIAL_TITLE,
    SPECIAL_UPPER,
};
use crate::generated::unicode_id::{ID_CONTINUE, ID_START};
use crate::generated::unicode_norm::{
    CANONICAL_DECOMPOSITION, COMBINING_CLASS, COMPATIBILITY_DECOMPOSITION, COMPOSITION,
};
use crate::generated::unicode_props::{
    BINARY_PROPERTIES, BP_CASE_IGNORABLE, BP_CASED, GENERAL_CATEGORY, GENERAL_CATEGORY_VALUES,
    SCRIPT, SCRIPT_EXTENSIONS, SCRIPT_VALUES, STRING_PROPERTIES,
};

/// Whether `c` is in a sorted table of inclusive ranges.
pub fn in_ranges(table: &[(u32, u32)], c: u32) -> bool {
    table
        .binary_search_by(|&(first, last)| {
            if last < c {
                std::cmp::Ordering::Less
            } else if first > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn in_table(table: &[(u32, u32)], c: char) -> bool {
    in_ranges(table, c as u32)
}

/// IdentifierStartChar: UnicodeIDStart, `$` or `_`.
pub fn is_id_start(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic() || c == '$' || c == '_'
    } else {
        in_table(ID_START, c)
    }
}

/// IdentifierPartChar: UnicodeIDContinue, `$`, ZWNJ or ZWJ.
pub fn is_id_continue(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric() || c == '$' || c == '_'
    } else {
        c == '\u{200C}' || c == '\u{200D}' || in_table(ID_CONTINUE, c)
    }
}

/// WhiteSpace: TAB, VT, FF, ZWNBSP and the Zs category. Zs is fixed since
/// Unicode 6.3 (U+180E left it) and is the same in 17.0.0.
pub fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\u{0B}' | '\u{0C}' | ' ' | '\u{A0}' | '\u{FEFF}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// LineTerminator: LF, CR, LS and PS.
pub fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// White space for `String.prototype.trim`: WhiteSpace or LineTerminator
/// (TrimString, ECMA-262 §22.1.3.32.1), on a UTF-16 code unit.
pub fn is_trim_space(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(|c| is_whitespace(c) || is_line_terminator(c))
}

// ----- UTF-16 -----

/// The code points of UTF-16 code units (CodePointAt, §11.1.4): paired
/// surrogates combine, lone surrogates stay as themselves.
pub fn code_points(units: &[u16]) -> Vec<u32> {
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let (cp, size) = code_point_at(units, i);
        out.push(cp);
        i += size;
    }
    out
}

/// CodePointAt(string, position): the code point at `i` and its size in
/// code units.
pub fn code_point_at(units: &[u16], i: usize) -> (u32, usize) {
    let first = units[i];
    if (0xD800..=0xDBFF).contains(&first)
        && let Some(&second) = units.get(i + 1)
        && (0xDC00..=0xDFFF).contains(&second)
    {
        let cp = 0x10000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00);
        return (cp, 2);
    }
    (u32::from(first), 1)
}

/// Appends UTF16EncodeCodePoint(cp); a surrogate code point is one unit.
pub fn push_utf16(out: &mut Vec<u16>, cp: u32) {
    if cp < 0x10000 {
        out.push(cp as u16);
    } else {
        let c = cp - 0x10000;
        out.push(0xD800 + (c >> 10) as u16);
        out.push(0xDC00 + (c & 0x3FF) as u16);
    }
}

/// The UTF-16 encoding of code points.
pub fn to_utf16(cps: &[u32]) -> Vec<u16> {
    let mut out = Vec::with_capacity(cps.len());
    for &cp in cps {
        push_utf16(&mut out, cp);
    }
    out
}

// ----- case -----

fn lookup(table: &[(u32, u32)], c: u32) -> Option<u32> {
    table
        .binary_search_by_key(&c, |&(k, _)| k)
        .ok()
        .map(|i| table[i].1)
}

fn lookup_multi(table: &'static [(u32, &'static [u32])], c: u32) -> Option<&'static [u32]> {
    table
        .binary_search_by_key(&c, |&(k, _)| k)
        .ok()
        .map(|i| table[i].1)
}

/// Simple_Lowercase_Mapping(c).
pub fn simple_lowercase(c: u32) -> u32 {
    lookup(SIMPLE_LOWER, c).unwrap_or(c)
}

/// Simple_Uppercase_Mapping(c).
pub fn simple_uppercase(c: u32) -> u32 {
    lookup(SIMPLE_UPPER, c).unwrap_or(c)
}

/// Simple_Titlecase_Mapping(c).
pub fn simple_titlecase(c: u32) -> u32 {
    lookup(SIMPLE_TITLE, c).unwrap_or(c)
}

fn push_full(
    out: &mut Vec<u32>,
    c: u32,
    special: &'static [(u32, &'static [u32])],
    simple: &[(u32, u32)],
) {
    match lookup_multi(special, c) {
        Some(m) => out.extend_from_slice(m),
        None => out.push(lookup(simple, c).unwrap_or(c)),
    }
}

/// Appends the full, language-insensitive Lowercase_Mapping of `c` out
/// of context (Final_Sigma needs [`to_lowercase`]).
pub fn push_lowercase(out: &mut Vec<u32>, c: u32) {
    push_full(out, c, SPECIAL_LOWER, SIMPLE_LOWER);
}

/// Appends the full, language-insensitive Uppercase_Mapping of `c`.
pub fn push_uppercase(out: &mut Vec<u32>, c: u32) {
    push_full(out, c, SPECIAL_UPPER, SIMPLE_UPPER);
}

/// Appends the full, language-insensitive Titlecase_Mapping of `c`.
pub fn push_titlecase(out: &mut Vec<u32>, c: u32) {
    push_full(out, c, SPECIAL_TITLE, SIMPLE_TITLE);
}

/// Whether `c` has the Cased property.
pub fn is_cased(c: u32) -> bool {
    in_ranges(BP_CASED, c)
}

/// Whether `c` has the Case_Ignorable property.
pub fn is_case_ignorable(c: u32) -> bool {
    in_ranges(BP_CASE_IGNORABLE, c)
}

/// The Final_Sigma condition (Unicode §3.13, table 3-17) for `cps[i]`: a
/// cased letter and then only case-ignorable characters come before it,
/// and no case-ignorable characters followed by a cased letter come after.
fn is_final_sigma(cps: &[u32], i: usize) -> bool {
    let before = cps[..i]
        .iter()
        .rev()
        .find(|&&c| !is_case_ignorable(c))
        .is_some_and(|&c| is_cased(c));
    if !before {
        return false;
    }
    let after = cps[i + 1..]
        .iter()
        .find(|&&c| !is_case_ignorable(c))
        .is_some_and(|&c| is_cased(c));
    !after
}

/// The full, language-insensitive lowercase of a code point sequence,
/// with the Final_Sigma rule of SpecialCasing.txt.
pub fn to_lowercase(cps: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(cps.len());
    for (i, &c) in cps.iter().enumerate() {
        if c < 0x80 {
            out.push(if (0x41..=0x5A).contains(&c) {
                c + 0x20
            } else {
                c
            });
        } else if c == 0x03A3 && is_final_sigma(cps, i) {
            out.push(0x03C2);
        } else {
            push_lowercase(&mut out, c);
        }
    }
    out
}

/// The full, language-insensitive uppercase of a code point sequence.
pub fn to_uppercase(cps: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(cps.len());
    for &c in cps {
        if c < 0x80 {
            out.push(if (0x61..=0x7A).contains(&c) {
                c - 0x20
            } else {
                c
            });
        } else {
            push_uppercase(&mut out, c);
        }
    }
    out
}

/// Simple case folding (CaseFolding.txt status C and S), the `scf` of
/// RegExp Canonicalize with the `u` or `v` flag.
pub fn simple_fold(c: u32) -> u32 {
    lookup(FOLD_SIMPLE, c).unwrap_or(c)
}

/// Appends the full case folding of `c` (status C and F).
pub fn push_full_fold(out: &mut Vec<u32>, c: u32) {
    match lookup_multi(FOLD_FULL, c) {
        Some(m) => out.extend_from_slice(m),
        // Every status S entry also has an F entry, so here the simple
        // folding is the C one.
        None => out.push(simple_fold(c)),
    }
}

// ----- normalization -----

/// The normalization forms of `String.prototype.normalize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizationForm {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

impl NormalizationForm {
    /// The form named `"NFC"`, `"NFD"`, `"NFKC"` or `"NFKD"`.
    pub fn from_name(name: &str) -> Option<NormalizationForm> {
        match name {
            "NFC" => Some(NormalizationForm::Nfc),
            "NFD" => Some(NormalizationForm::Nfd),
            "NFKC" => Some(NormalizationForm::Nfkc),
            "NFKD" => Some(NormalizationForm::Nfkd),
            _ => None,
        }
    }
}

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// Canonical_Combining_Class(c).
pub fn combining_class(c: u32) -> u8 {
    match COMBINING_CLASS.binary_search_by(|&(first, last, _)| {
        if last < c {
            std::cmp::Ordering::Less
        } else if first > c {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => COMBINING_CLASS[i].2,
        Err(_) => 0,
    }
}

/// Appends the full canonical (or, with `compat`, compatibility)
/// decomposition of `c`.
fn push_decomposition(out: &mut Vec<u32>, c: u32, compat: bool) {
    if (S_BASE..S_BASE + S_COUNT).contains(&c) {
        let s = c - S_BASE;
        out.push(L_BASE + s / N_COUNT);
        out.push(V_BASE + (s % N_COUNT) / T_COUNT);
        if s % T_COUNT != 0 {
            out.push(T_BASE + s % T_COUNT);
        }
        return;
    }
    let table = if compat {
        COMPATIBILITY_DECOMPOSITION
    } else {
        CANONICAL_DECOMPOSITION
    };
    match lookup_multi(table, c) {
        Some(m) => out.extend_from_slice(m),
        None => out.push(c),
    }
}

/// The canonical ordering algorithm: a stable sort by combining class of
/// every run of non-starters.
fn canonical_order(cps: &mut [u32]) {
    let mut i = 0;
    while i < cps.len() {
        if combining_class(cps[i]) == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < cps.len() && combining_class(cps[i]) != 0 {
            i += 1;
        }
        cps[start..i].sort_by_key(|&c| combining_class(c));
    }
}

/// The primary composite of `a` and `b`, if any.
fn compose_pair(a: u32, b: u32) -> Option<u32> {
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return Some(S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a)
        && (a - S_BASE) % T_COUNT == 0
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&b)
    {
        return Some(a + (b - T_BASE));
    }
    COMPOSITION
        .binary_search_by(|&(x, y, _)| (x, y).cmp(&(a, b)))
        .ok()
        .map(|i| COMPOSITION[i].2)
}

/// The canonical composition algorithm on a decomposed, canonically
/// ordered sequence.
fn compose(cps: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(cps.len());
    let mut starter: Option<usize> = None;
    // Class of the last character kept after the starter; `None` when the
    // starter is the last one kept.
    let mut last_class: Option<u8> = None;
    for &c in cps {
        let class = combining_class(c);
        if let Some(s) = starter {
            let blocked = last_class.is_some_and(|l| l == 0 || l >= class);
            if !blocked && let Some(composite) = compose_pair(out[s], c) {
                out[s] = composite;
                continue;
            }
        }
        if class == 0 {
            starter = Some(out.len());
            last_class = None;
        } else {
            last_class = Some(class);
        }
        out.push(c);
    }
    out
}

/// The code points of `cps` in normalization form `form` (UAX #15).
pub fn normalize(cps: &[u32], form: NormalizationForm) -> Vec<u32> {
    if cps.iter().all(|&c| c < 0x300) && form == NormalizationForm::Nfc {
        // Below U+0300 nothing decomposes canonically into something that
        // composes differently, except the precomposed Latin-1 letters,
        // which are already NFC.
        return cps.to_vec();
    }
    let compat = matches!(form, NormalizationForm::Nfkc | NormalizationForm::Nfkd);
    let mut decomposed = Vec::with_capacity(cps.len());
    for &c in cps {
        push_decomposition(&mut decomposed, c, compat);
    }
    canonical_order(&mut decomposed);
    match form {
        NormalizationForm::Nfd | NormalizationForm::Nfkd => decomposed,
        NormalizationForm::Nfc | NormalizationForm::Nfkc => compose(&decomposed),
    }
}

/// [`normalize`] on UTF-16 code units.
pub fn normalize_utf16(units: &[u16], form: NormalizationForm) -> Vec<u16> {
    to_utf16(&normalize(&code_points(units), form))
}

// ----- properties -----

fn range_value<T: Copy>(table: &[(u32, u32, T)], c: u32) -> Option<T> {
    table
        .binary_search_by(|&(first, last, _)| {
            if last < c {
                std::cmp::Ordering::Less
            } else if first > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .ok()
        .map(|i| table[i].2)
}

/// The ranges of the binary property named `name` (its canonical name or
/// alias in ECMA-262 §22.2.2.9, case-sensitive).
pub fn binary_property(name: &str) -> Option<&'static [(u32, u32)]> {
    BINARY_PROPERTIES
        .iter()
        .find(|&&(n, alias, _)| n == name || (!alias.is_empty() && alias == name))
        .map(|&(_, _, ranges)| ranges)
}

/// Whether `c` has the binary property `name`; `None` if there is no such
/// property.
pub fn has_binary_property(name: &str, c: u32) -> Option<bool> {
    binary_property(name).map(|ranges| in_ranges(ranges, c))
}

/// The short General_Category value of `c` (`"Lu"`, `"Cn"`, …).
pub fn general_category(c: u32) -> &'static str {
    range_value(GENERAL_CATEGORY, c).unwrap_or("Cn")
}

/// The code points of the General_Category value or group named `value`
/// (any alias of PropertyValueAliases.txt).
pub fn general_category_set(value: &str) -> Option<&'static [(u32, u32)]> {
    GENERAL_CATEGORY_VALUES
        .iter()
        .find(|(names, _)| names.contains(&value))
        .map(|&(_, ranges)| ranges)
}

/// The long Script value of `c` (`"Latin"`, `"Unknown"`, …).
pub fn script(c: u32) -> &'static str {
    range_value(SCRIPT, c).unwrap_or("Unknown")
}

/// The long Script_Extensions values of `c`.
pub fn script_extensions(c: u32) -> Vec<&'static str> {
    match range_value(SCRIPT_EXTENSIONS, c) {
        Some(names) => names.to_vec(),
        None => vec![script(c)],
    }
}

/// The code points whose Script (or, with `extensions`,
/// Script_Extensions) is `value` (any alias).
pub fn script_set(value: &str, extensions: bool) -> Option<&'static [(u32, u32)]> {
    SCRIPT_VALUES
        .iter()
        .find(|(names, _, _)| names.contains(&value))
        .map(|&(_, sc, scx)| if extensions { scx } else { sc })
}

/// A property of strings (ECMA-262 §22.2.2.9): its single code points and
/// its sequences of two or more code points.
#[derive(Clone, Copy, Debug)]
pub struct StringProperty {
    pub code_points: &'static [(u32, u32)],
    pub strings: &'static [&'static [u32]],
}

/// The property of strings named `name`.
pub fn string_property(name: &str) -> Option<StringProperty> {
    STRING_PROPERTIES
        .iter()
        .find(|&&(n, _, _)| n == name)
        .map(|&(_, code_points, strings)| StringProperty {
            code_points,
            strings,
        })
}

/// What a `\p{…}` property expression names.
#[derive(Clone, Copy, Debug)]
pub enum PropertyMatch {
    CodePoints(&'static [(u32, u32)]),
    /// A property of strings; only valid with the `v` flag.
    Strings(StringProperty),
}

/// Resolves `\p{name=value}` (`name` given) or `\p{value}` as ECMA-262
/// UnicodeMatchProperty and UnicodeMatchPropertyValue do: names and values
/// are case-sensitive and only the aliases of the tables count. `None`
/// means a SyntaxError.
pub fn property_escape(name: Option<&str>, value: &str) -> Option<PropertyMatch> {
    let set = match name {
        Some("General_Category" | "gc") => general_category_set(value),
        Some("Script" | "sc") => script_set(value, false),
        Some("Script_Extensions" | "scx") => script_set(value, true),
        Some(_) => None,
        None => {
            if let Some(set) = general_category_set(value).or_else(|| binary_property(value)) {
                Some(set)
            } else {
                return string_property(value).map(PropertyMatch::Strings);
            }
        }
    };
    set.map(PropertyMatch::CodePoints)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_identifier_chars() {
        assert!(is_id_start('a') && is_id_start('Z') && is_id_start('$') && is_id_start('_'));
        assert!(!is_id_start('1') && !is_id_start('-') && !is_id_start('#'));
        assert!(is_id_continue('1') && is_id_continue('_') && !is_id_continue(' '));
    }

    #[test]
    fn unicode_identifier_chars() {
        // Greek, CJK and an astral letter (MATHEMATICAL BOLD CAPITAL A).
        assert!(is_id_start('λ') && is_id_start('中') && is_id_start('\u{1D400}'));
        // Combining acute accent: ID_Continue but not ID_Start.
        assert!(!is_id_start('\u{0301}') && is_id_continue('\u{0301}'));
        assert!(is_id_continue('\u{200C}') && is_id_continue('\u{200D}'));
        assert!(!is_id_start('\u{200C}'));
        // VERTICAL TILDE is Pattern_Syntax, excluded from ID_Start.
        assert!(!is_id_start('\u{2E2F}'));
        // U+1885 and U+1886 are Other_ID_Start.
        assert!(is_id_start('\u{1885}') && is_id_start('\u{1886}'));
        assert!(!is_id_start('\u{10FFFF}'));
    }

    #[test]
    fn whitespace_and_line_terminators() {
        for c in [
            '\t', '\u{0B}', '\u{0C}', ' ', '\u{A0}', '\u{FEFF}', '\u{3000}',
        ] {
            assert!(is_whitespace(c));
        }
        assert!(!is_whitespace('\u{180E}') && !is_whitespace('\n'));
        for c in ['\n', '\r', '\u{2028}', '\u{2029}'] {
            assert!(is_line_terminator(c));
        }
    }

    #[test]
    fn utf16_round_trip() {
        let units = [0x61, 0xD83D, 0xDE00, 0xDC00];
        assert_eq!(code_points(&units), vec![0x61, 0x1F600, 0xDC00]);
        assert_eq!(to_utf16(&code_points(&units)), units);
        assert!(is_trim_space(0x2028) && is_trim_space(0xFEFF) && !is_trim_space(0x180E));
    }

    #[test]
    fn hangul_and_composition() {
        assert_eq!(
            normalize(&[0xAC01], NormalizationForm::Nfd),
            vec![0x1100, 0x1161, 0x11A8]
        );
        assert_eq!(
            normalize(&[0x1100, 0x1161, 0x11A8], NormalizationForm::Nfc),
            vec![0xAC01]
        );
        // U+0301 after U+0316 (class 220) still composes with the starter;
        // after U+0305 (class 230, no composite with a) it is blocked.
        assert_eq!(
            normalize(&[0x61, 0x316, 0x301], NormalizationForm::Nfc),
            vec![0xE1, 0x316]
        );
        assert_eq!(
            normalize(&[0x61, 0x305, 0x301], NormalizationForm::Nfc),
            vec![0x61, 0x305, 0x301]
        );
    }

    #[test]
    fn case_and_properties() {
        assert_eq!(to_uppercase(&[0xDF]), vec![0x53, 0x53]);
        assert_eq!(to_lowercase(&[0x41, 0x3A3]), vec![0x61, 0x3C2]);
        assert_eq!(general_category(0x41), "Lu");
        assert!(matches!(
            property_escape(Some("Script"), "Latin"),
            Some(PropertyMatch::CodePoints(_))
        ));
        assert!(matches!(
            property_escape(None, "Basic_Emoji"),
            Some(PropertyMatch::Strings(_))
        ));
    }
}
