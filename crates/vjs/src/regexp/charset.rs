//! Character sets as sorted ranges, and Canonicalize (ECMA-262
//! §22.2.2.7.3) with the case closure the character class matchers need.
//!
//! A "character" is a code unit without the `u` and `v` flags and a code
//! point with them; both are `u32` here.

use std::sync::OnceLock;

use crate::generated::unicode_case::{FOLD_SIMPLE, SIMPLE_UPPER, SPECIAL_UPPER};
use crate::unicode;

/// Largest character without the `u` and `v` flags (a code unit).
pub const MAX_CODE_UNIT: u32 = 0xFFFF;
/// Largest character with the `u` or `v` flag (a code point).
pub const MAX_CODE_POINT: u32 = 0x10FFFF;

/// A set of characters: sorted, disjoint, non-adjacent inclusive ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CharSet {
    ranges: Vec<(u32, u32)>,
}

impl CharSet {
    pub fn new() -> CharSet {
        CharSet { ranges: Vec::new() }
    }

    pub fn single(c: u32) -> CharSet {
        CharSet {
            ranges: vec![(c, c)],
        }
    }

    pub fn range(first: u32, last: u32) -> CharSet {
        CharSet {
            ranges: vec![(first, last)],
        }
    }

    /// A set from ranges in any order, possibly overlapping.
    pub fn from_ranges(ranges: &[(u32, u32)]) -> CharSet {
        let mut v: Vec<(u32, u32)> = ranges.to_vec();
        v.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
        for (first, last) in v {
            match out.last_mut() {
                Some(prev) if first <= prev.1.saturating_add(1) => {
                    prev.1 = prev.1.max(last);
                }
                _ => out.push((first, last)),
            }
        }
        CharSet { ranges: out }
    }

    pub fn ranges(&self) -> &[(u32, u32)] {
        &self.ranges
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn contains(&self, c: u32) -> bool {
        unicode::in_ranges(&self.ranges, c)
    }

    /// The only character of a one-character set.
    pub fn as_single(&self) -> Option<u32> {
        match self.ranges.as_slice() {
            [(a, b)] if a == b => Some(*a),
            _ => None,
        }
    }

    pub fn add(&mut self, c: u32) {
        self.add_range(c, c);
    }

    pub fn add_range(&mut self, first: u32, last: u32) {
        if let Some(prev) = self.ranges.last_mut() {
            // Fast path: appending in order.
            if first > prev.1.saturating_add(1) {
                self.ranges.push((first, last));
                return;
            }
            if first >= prev.0 {
                prev.1 = prev.1.max(last);
                return;
            }
        } else {
            self.ranges.push((first, last));
            return;
        }
        *self = self.union(&CharSet::range(first, last));
    }

    pub fn add_set(&mut self, other: &CharSet) {
        *self = self.union(other);
    }

    pub fn union(&self, other: &CharSet) -> CharSet {
        let mut all = Vec::with_capacity(self.ranges.len() + other.ranges.len());
        all.extend_from_slice(&self.ranges);
        all.extend_from_slice(&other.ranges);
        CharSet::from_ranges(&all)
    }

    pub fn intersect(&self, other: &CharSet) -> CharSet {
        let (a, b) = (&self.ranges, &other.ranges);
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        while i < a.len() && j < b.len() {
            let first = a[i].0.max(b[j].0);
            let last = a[i].1.min(b[j].1);
            if first <= last {
                out.push((first, last));
            }
            if a[i].1 < b[j].1 {
                i += 1;
            } else {
                j += 1;
            }
        }
        CharSet { ranges: out }
    }

    /// The characters of `0..=max` not in the set.
    pub fn complement(&self, max: u32) -> CharSet {
        let mut out = Vec::new();
        let mut next = 0u32;
        for &(first, last) in &self.ranges {
            if first > max {
                break;
            }
            if first > next {
                out.push((next, first - 1));
            }
            next = last.saturating_add(1);
            if last >= max {
                next = max + 1;
                break;
            }
        }
        if next <= max {
            out.push((next, max));
        }
        CharSet { ranges: out }
    }

    pub fn subtract(&self, other: &CharSet) -> CharSet {
        self.intersect(&other.complement(MAX_CODE_POINT))
    }
}

/// Which Canonicalize applies: the legacy one (`toUppercase`, without the
/// `u` and `v` flags) or simple case folding (with either flag).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseFold {
    Legacy,
    Unicode,
}

impl CaseFold {
    pub fn for_unicode(unicode: bool) -> CaseFold {
        if unicode {
            CaseFold::Unicode
        } else {
            CaseFold::Legacy
        }
    }
}

/// The characters whose Canonicalize differs from themselves: `(c,
/// Canonicalize(c))` sorted by `c`, and those `c` as a set.
struct CaseTable {
    pairs: Vec<(u32, u32)>,
    changed: CharSet,
}

impl CaseTable {
    fn build(mut pairs: Vec<(u32, u32)>) -> CaseTable {
        pairs.sort_unstable();
        pairs.dedup();
        let changed = CharSet::from_ranges(&pairs.iter().map(|&(c, _)| (c, c)).collect::<Vec<_>>());
        CaseTable { pairs, changed }
    }

    fn lookup(&self, c: u32) -> u32 {
        match self.pairs.binary_search_by_key(&c, |&(k, _)| k) {
            Ok(i) => self.pairs[i].1,
            Err(_) => c,
        }
    }
}

fn legacy_table() -> &'static CaseTable {
    static TABLE: OnceLock<CaseTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut candidates: Vec<u32> = SIMPLE_UPPER.iter().map(|&(c, _)| c).collect();
        candidates.extend(SPECIAL_UPPER.iter().map(|&(c, _)| c));
        let mut pairs = Vec::new();
        let mut buf = Vec::new();
        for c in candidates {
            if c > MAX_CODE_UNIT {
                continue;
            }
            buf.clear();
            unicode::push_uppercase(&mut buf, c);
            // Canonicalize without u/v: keep ch if toUppercase is not one
            // code unit or maps a non-ASCII character into ASCII.
            if buf.len() != 1 {
                continue;
            }
            let u = buf[0];
            if u > MAX_CODE_UNIT || (c >= 128 && u < 128) || u == c {
                continue;
            }
            pairs.push((c, u));
        }
        CaseTable::build(pairs)
    })
}

fn unicode_table() -> &'static CaseTable {
    static TABLE: OnceLock<CaseTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        CaseTable::build(
            FOLD_SIMPLE
                .iter()
                .copied()
                .filter(|&(c, f)| c != f)
                .collect(),
        )
    })
}

fn table(fold: CaseFold) -> &'static CaseTable {
    match fold {
        CaseFold::Legacy => legacy_table(),
        CaseFold::Unicode => unicode_table(),
    }
}

/// Canonicalize(rer, ch) for a pattern with `IgnoreCase`.
pub fn canonicalize(c: u32, fold: CaseFold) -> u32 {
    if c < 128 {
        return match fold {
            CaseFold::Legacy if (0x61..=0x7A).contains(&c) => c - 0x20,
            CaseFold::Unicode if (0x41..=0x5A).contains(&c) => c + 0x20,
            _ => c,
        };
    }
    table(fold).lookup(c)
}

/// `{ Canonicalize(c) | c ∈ set }`: a class matcher with `IgnoreCase`
/// tests `Canonicalize(ch)` against this set, which is the same as asking
/// whether some member of `set` canonicalizes like `ch`.
pub fn canonical_image(set: &CharSet, fold: CaseFold) -> CharSet {
    let t = table(fold);
    let mut out = set.subtract(&t.changed);
    let mut added: Vec<(u32, u32)> = Vec::new();
    for &(c, k) in &t.pairs {
        if set.contains(c) {
            added.push((k, k));
        }
    }
    if !added.is_empty() {
        out = out.union(&CharSet::from_ranges(&added));
    }
    out
}

/// The code points that have no simple case folding (`scf(c) = c`):
/// AllCharacters with the `v` and `i` flags.
pub fn canonical_code_points() -> &'static CharSet {
    static SET: OnceLock<CharSet> = OnceLock::new();
    SET.get_or_init(|| CharSet::range(0, MAX_CODE_POINT).subtract(&unicode_table().changed))
}

/// LineTerminator (§12.3).
pub fn is_line_terminator(c: u32) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

/// The basic word characters `[A-Za-z0-9_]`.
pub fn is_basic_word_char(c: u32) -> bool {
    c < 128 && (c as u8).is_ascii_alphanumeric() || c == u32::from(b'_')
}

/// WordCharacters(rer): with `IgnoreCase` and either Unicode flag it also
/// holds U+017F and U+212A, which fold into `s` and `k`.
pub fn is_word_char(c: u32, unicode_ignore_case: bool) -> bool {
    is_basic_word_char(c) || (unicode_ignore_case && (c == 0x017F || c == 0x212A))
}

pub fn word_chars(unicode_ignore_case: bool) -> CharSet {
    let mut s = CharSet::from_ranges(&[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)]);
    if unicode_ignore_case {
        s = s.union(&CharSet::from_ranges(&[(0x017F, 0x017F), (0x212A, 0x212A)]));
    }
    s
}

pub fn digit_chars() -> CharSet {
    CharSet::range(0x30, 0x39)
}

/// WhiteSpace and LineTerminator code points (`\s`).
pub fn space_chars() -> CharSet {
    CharSet::from_ranges(&[
        (0x09, 0x0D),
        (0x20, 0x20),
        (0xA0, 0xA0),
        (0x1680, 0x1680),
        (0x2000, 0x200A),
        (0x2028, 0x2029),
        (0x202F, 0x202F),
        (0x205F, 0x205F),
        (0x3000, 0x3000),
        (0xFEFF, 0xFEFF),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_operations() {
        let a = CharSet::from_ranges(&[(10, 20), (30, 40)]);
        let b = CharSet::from_ranges(&[(15, 35)]);
        assert_eq!(a.union(&b).ranges(), &[(10, 40)]);
        assert_eq!(a.intersect(&b).ranges(), &[(15, 20), (30, 35)]);
        assert_eq!(a.subtract(&b).ranges(), &[(10, 14), (36, 40)]);
        assert_eq!(a.complement(50).ranges(), &[(0, 9), (21, 29), (41, 50)]);
        assert_eq!(CharSet::new().complement(5).ranges(), &[(0, 5)]);
        assert_eq!(
            CharSet::range(0, 5).complement(5).ranges(),
            &[] as &[(u32, u32)]
        );
        let mut c = CharSet::new();
        c.add(5);
        c.add(3);
        c.add(4);
        assert_eq!(c.ranges(), &[(3, 5)]);
    }

    #[test]
    fn canonicalize_modes() {
        assert_eq!(canonicalize('a' as u32, CaseFold::Legacy), 'A' as u32);
        assert_eq!(canonicalize('A' as u32, CaseFold::Unicode), 'a' as u32);
        // ſ uppercases to S, but a non-ASCII character never maps into
        // ASCII without u/v; with u it folds to s.
        assert_eq!(canonicalize(0x017F, CaseFold::Legacy), 0x017F);
        assert_eq!(canonicalize(0x017F, CaseFold::Unicode), 's' as u32);
        // ß uppercases to "SS" (two code units): unchanged.
        assert_eq!(canonicalize(0xDF, CaseFold::Legacy), 0xDF);
        // K (Kelvin) folds to k.
        assert_eq!(canonicalize(0x212A, CaseFold::Unicode), 'k' as u32);
        let img = canonical_image(&CharSet::range('a' as u32, 'c' as u32), CaseFold::Unicode);
        assert_eq!(img.ranges(), &[('a' as u32, 'c' as u32)]);
        let img = canonical_image(&CharSet::range('A' as u32, 'C' as u32), CaseFold::Unicode);
        assert_eq!(img.ranges(), &[('a' as u32, 'c' as u32)]);
    }
}
