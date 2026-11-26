//! The Unicode Bidirectional Algorithm (UAX #9) for one paragraph: P2 and
//! P3, the explicit levels and directions (X1 to X10, with isolating run
//! sequences), the weak types (W1 to W7), paired brackets (N0), neutral
//! types (N1, N2), implicit levels (I1, I2), and per line the reset of
//! trailing white space (L1) and the visual order (L2). Bidi_Class comes
//! from `generated/idna.rs` and the brackets and mirrored glyphs from
//! `generated/bidi.rs` (Unicode 17.0.0).
//!
//! CSS `direction` and `unicode-bidi` reach the algorithm as the
//! equivalent control characters (css-writing-modes-4 §2.4), which the
//! caller inserts as extra entries of the class array.

use crate::generated::bidi::{BRACKETS, MIRRORING};
use crate::generated::idna::BIDI_CLASS;
pub use crate::generated::idna::{
    BIDI_AL as AL, BIDI_AN as AN, BIDI_B as B, BIDI_BN as BN, BIDI_CS as CS, BIDI_EN as EN,
    BIDI_ES as ES, BIDI_ET as ET, BIDI_FSI as FSI, BIDI_L as L, BIDI_LRE as LRE, BIDI_LRI as LRI,
    BIDI_LRO as LRO, BIDI_NSM as NSM, BIDI_ON as ON, BIDI_PDF as PDF, BIDI_PDI as PDI, BIDI_R as R,
    BIDI_RLE as RLE, BIDI_RLI as RLI, BIDI_RLO as RLO, BIDI_S as S, BIDI_WS as WS,
};

const MAX_DEPTH: u8 = 125;

/// The Bidi_Class of `c`.
pub fn class_of(c: u32) -> u8 {
    match BIDI_CLASS.binary_search_by(|&(a, b, _)| {
        if b < c {
            std::cmp::Ordering::Less
        } else if a > c {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => BIDI_CLASS[i].2,
        Err(_) => L,
    }
}

/// Bidi_Mirroring_Glyph of `c`.
pub fn mirror(c: u32) -> Option<u32> {
    MIRRORING
        .binary_search_by_key(&c, |p| p.0)
        .ok()
        .map(|i| MIRRORING[i].1)
}

/// Bidi_Paired_Bracket of `c` and whether it opens.
fn bracket(c: u32) -> Option<(u32, bool)> {
    BRACKETS
        .binary_search_by_key(&c, |p| p.0)
        .ok()
        .map(|i| (BRACKETS[i].1, BRACKETS[i].2))
}

/// U+2329 and U+232A are canonically equivalent to U+3008 and U+3009.
fn canonical_bracket(c: u32) -> u32 {
    match c {
        0x2329 => 0x3008,
        0x232A => 0x3009,
        x => x,
    }
}

fn is_isolate_initiator(c: u8) -> bool {
    matches!(c, LRI | RLI | FSI)
}

fn is_removed(c: u8) -> bool {
    matches!(c, RLE | LRE | RLO | LRO | PDF | BN)
}

fn is_strong(c: u8) -> bool {
    matches!(c, L | R | AL)
}

/// P2 and P3: the level of the first strong character of
/// `classes[from..to]`, skipping isolates; `None` without one.
fn first_strong(classes: &[u8], from: usize, to: usize) -> Option<u8> {
    let mut depth = 0usize;
    for &c in &classes[from..to] {
        match c {
            LRI | RLI | FSI => depth += 1,
            PDI => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            L if depth == 0 => return Some(0),
            R | AL if depth == 0 => return Some(1),
            B if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// The index of the PDI matching each isolate initiator (BD9).
fn matching_pdis(classes: &[u8]) -> Vec<Option<usize>> {
    let n = classes.len();
    let mut out = vec![None; n];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..n {
        match classes[i] {
            LRI | RLI | FSI => stack.push(i),
            PDI => {
                if let Some(open) = stack.pop() {
                    out[open] = Some(i);
                }
            }
            B => stack.clear(),
            _ => {}
        }
    }
    out
}

/// The resolved levels of a paragraph.
#[derive(Clone, Debug)]
pub struct Paragraph {
    pub level: u8,
    /// One level per input entry.
    pub levels: Vec<u8>,
    /// The original classes (for L1).
    pub classes: Vec<u8>,
}

#[derive(Clone, Copy)]
struct Entry {
    level: u8,
    override_: Option<u8>,
    isolate: bool,
}

/// Resolves the levels of `classes` (one per character or control) with
/// `chars` (the code points, for paired brackets; 0 for entries that are
/// not characters) and the paragraph level `base` (`None` = P2 and P3).
pub fn resolve(classes: &[u8], chars: &[u32], base: Option<u8>) -> Paragraph {
    let n = classes.len();
    let para = base.unwrap_or_else(|| first_strong(classes, 0, n).unwrap_or(0));
    let original = classes.to_vec();
    let mut types = classes.to_vec();
    let mut levels = vec![para; n];
    if n == 0 {
        return Paragraph {
            level: para,
            levels,
            classes: original,
        };
    }
    let pdis = matching_pdis(classes);

    // X1 to X8.
    let mut stack: Vec<Entry> = vec![Entry {
        level: para,
        override_: None,
        isolate: false,
    }];
    let mut overflow_isolate = 0usize;
    let mut overflow_embedding = 0usize;
    let mut valid_isolate = 0usize;
    for i in 0..n {
        let top = *stack.last().expect("the stack has the paragraph entry");
        match original[i] {
            RLE | LRE | RLO | LRO => {
                levels[i] = top.level;
                let rtl = matches!(original[i], RLE | RLO);
                let level = if rtl {
                    (top.level + 1) | 1
                } else {
                    (top.level + 2) & !1
                };
                if level <= MAX_DEPTH && overflow_isolate == 0 && overflow_embedding == 0 {
                    stack.push(Entry {
                        level,
                        override_: match original[i] {
                            RLO => Some(R),
                            LRO => Some(L),
                            _ => None,
                        },
                        isolate: false,
                    });
                } else if overflow_isolate == 0 {
                    overflow_embedding += 1;
                }
            }
            RLI | LRI | FSI => {
                levels[i] = top.level;
                if let Some(o) = top.override_ {
                    types[i] = o;
                }
                let rtl = match original[i] {
                    RLI => true,
                    LRI => false,
                    _ => {
                        let end = pdis[i].unwrap_or(n);
                        first_strong(classes, i + 1, end) == Some(1)
                    }
                };
                let level = if rtl {
                    (top.level + 1) | 1
                } else {
                    (top.level + 2) & !1
                };
                if level <= MAX_DEPTH && overflow_isolate == 0 && overflow_embedding == 0 {
                    valid_isolate += 1;
                    stack.push(Entry {
                        level,
                        override_: None,
                        isolate: true,
                    });
                } else {
                    overflow_isolate += 1;
                }
            }
            PDI => {
                if overflow_isolate > 0 {
                    overflow_isolate -= 1;
                } else if valid_isolate > 0 {
                    overflow_embedding = 0;
                    while stack.last().is_some_and(|e| !e.isolate) && stack.len() > 1 {
                        stack.pop();
                    }
                    if stack.len() > 1 {
                        stack.pop();
                    }
                    valid_isolate -= 1;
                }
                let top = *stack.last().expect("the stack has the paragraph entry");
                levels[i] = top.level;
                if let Some(o) = top.override_ {
                    types[i] = o;
                }
            }
            PDF => {
                levels[i] = top.level;
                if overflow_isolate == 0 {
                    if overflow_embedding > 0 {
                        overflow_embedding -= 1;
                    } else if !top.isolate && stack.len() > 1 {
                        stack.pop();
                    }
                }
            }
            B => {
                levels[i] = para;
            }
            BN => levels[i] = top.level,
            _ => {
                levels[i] = top.level;
                if let Some(o) = top.override_ {
                    types[i] = o;
                }
            }
        }
    }

    // X9: the removed entries are skipped from here on.
    let kept: Vec<usize> = (0..n).filter(|&i| !is_removed(original[i])).collect();
    // X10: level runs, then isolating run sequences.
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for &i in &kept {
        match runs.last_mut() {
            Some(r) if levels[*r.last().expect("runs are not empty")] == levels[i] => r.push(i),
            _ => runs.push(vec![i]),
        }
    }
    let run_of_start: std::collections::HashMap<usize, usize> =
        runs.iter().enumerate().map(|(k, r)| (r[0], k)).collect();
    let mut sequences: Vec<Vec<usize>> = Vec::new();
    for (k, r) in runs.iter().enumerate() {
        // A run that starts with a PDI matching an initiator continues a
        // sequence.
        let first = r[0];
        if original[first] == PDI && pdis.iter().any(|p| *p == Some(first)) {
            continue;
        }
        let mut seq = r.clone();
        let mut current = k;
        loop {
            let last = *runs[current].last().expect("runs are not empty");
            if !is_isolate_initiator(original[last]) {
                break;
            }
            let Some(pdi) = pdis[last] else {
                break;
            };
            let Some(&next) = run_of_start.get(&pdi) else {
                break;
            };
            seq.extend_from_slice(&runs[next]);
            current = next;
        }
        sequences.push(seq);
    }

    for seq in &sequences {
        resolve_sequence(seq, &original, &mut types, &levels, &pdis, chars, para, n);
    }

    // I1 and I2.
    for &i in &kept {
        let t = types[i];
        let lv = levels[i];
        if lv % 2 == 0 {
            if t == R {
                levels[i] = lv + 1;
            } else if t == AN || t == EN {
                levels[i] = lv + 2;
            }
        } else if t == L || t == EN || t == AN {
            levels[i] = lv + 1;
        }
    }
    // Removed entries take the level of the entry before them.
    let mut prev = para;
    for i in 0..n {
        if is_removed(original[i]) {
            levels[i] = prev;
        } else {
            prev = levels[i];
        }
    }
    Paragraph {
        level: para,
        levels,
        classes: original,
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_sequence(
    seq: &[usize],
    original: &[u8],
    types: &mut [u8],
    levels: &[u8],
    pdis: &[Option<usize>],
    chars: &[u32],
    para: u8,
    n: usize,
) {
    let level = levels[seq[0]];
    // sos and eos.
    let before = (0..seq[0])
        .rev()
        .find(|&j| !is_removed(original[j]))
        .map_or(para, |j| levels[j]);
    let last = *seq.last().expect("sequences are not empty");
    // A sequence that ends with an isolate initiator ends at the
    // paragraph level.
    let after = if is_isolate_initiator(original[last]) {
        para
    } else {
        (last + 1..n)
            .find(|&j| !is_removed(original[j]))
            .map_or(para, |j| levels[j])
    };
    let dir = |lv: u8| if lv % 2 == 0 { L } else { R };
    let sos = dir(level.max(before));
    let eos = dir(level.max(after));
    let m = seq.len();

    // W1
    let mut prev = sos;
    for k in 0..m {
        let i = seq[k];
        if types[i] == NSM {
            types[i] = if is_isolate_initiator(prev) || prev == PDI {
                ON
            } else {
                prev
            };
        }
        prev = types[i];
    }
    // W2 and W3
    let mut last_strong = sos;
    for k in 0..m {
        let i = seq[k];
        match types[i] {
            L | R | AL => last_strong = types[i],
            EN if last_strong == AL => types[i] = AN,
            _ => {}
        }
    }
    for k in 0..m {
        if types[seq[k]] == AL {
            types[seq[k]] = R;
        }
    }
    // W4
    for k in 1..m.saturating_sub(1) {
        let (a, c, b) = (types[seq[k - 1]], types[seq[k]], types[seq[k + 1]]);
        if c == ES && a == EN && b == EN {
            types[seq[k]] = EN;
        } else if c == CS && a == b && (a == EN || a == AN) {
            types[seq[k]] = a;
        }
    }
    // W5
    let mut k = 0;
    while k < m {
        if types[seq[k]] == ET {
            let start = k;
            while k < m && types[seq[k]] == ET {
                k += 1;
            }
            let touches =
                (start > 0 && types[seq[start - 1]] == EN) || (k < m && types[seq[k]] == EN);
            if touches {
                for j in start..k {
                    types[seq[j]] = EN;
                }
            }
        } else {
            k += 1;
        }
    }
    // W6
    for k in 0..m {
        if matches!(types[seq[k]], ES | ET | CS) {
            types[seq[k]] = ON;
        }
    }
    // W7
    let mut last_strong = sos;
    for k in 0..m {
        let i = seq[k];
        match types[i] {
            L | R => last_strong = types[i],
            EN if last_strong == L => types[i] = L,
            _ => {}
        }
    }

    // N0: paired brackets.
    let e = dir(level);
    let strong_of = |x: u8| match x {
        L => Some(L),
        R | EN | AN => Some(R),
        _ => None,
    };
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut open: Vec<(u32, usize)> = Vec::new();
    for k in 0..m {
        let i = seq[k];
        if types[i] != ON {
            continue;
        }
        let c = chars.get(i).copied().unwrap_or(0);
        let Some((pair, opening)) = bracket(c) else {
            continue;
        };
        if opening {
            if open.len() == 63 {
                break;
            }
            open.push((canonical_bracket(pair), k));
        } else if let Some(pos) = open.iter().rposition(|&(p, _)| p == canonical_bracket(c)) {
            pairs.push((open[pos].1, k));
            open.truncate(pos);
        }
    }
    pairs.sort();
    for &(o, c) in &pairs {
        let mut found_e = false;
        let mut found_opposite = false;
        for k in o + 1..c {
            match strong_of(types[seq[k]]) {
                Some(s) if s == e => found_e = true,
                Some(_) => found_opposite = true,
                None => {}
            }
        }
        let resolved = if found_e {
            Some(e)
        } else if found_opposite {
            let mut context = sos;
            for k in (0..o).rev() {
                if let Some(s) = strong_of(types[seq[k]]) {
                    context = s;
                    break;
                }
            }
            Some(if context != e { context } else { e })
        } else {
            None
        };
        if let Some(d) = resolved {
            types[seq[o]] = d;
            types[seq[c]] = d;
            for idx in [o, c] {
                let mut k = idx + 1;
                while k < m && original[seq[k]] == NSM {
                    types[seq[k]] = d;
                    k += 1;
                }
            }
        }
    }

    // N1 and N2.
    let is_ni = |x: u8| matches!(x, B | S | WS | ON | LRI | RLI | FSI | PDI);
    let mut k = 0;
    while k < m {
        if !is_ni(types[seq[k]]) {
            k += 1;
            continue;
        }
        let start = k;
        while k < m && is_ni(types[seq[k]]) {
            k += 1;
        }
        let before = if start == 0 {
            sos
        } else {
            strong_of(types[seq[start - 1]]).unwrap_or(e)
        };
        let after = if k == m {
            eos
        } else {
            strong_of(types[seq[k]]).unwrap_or(e)
        };
        let d = if before == after { before } else { e };
        for j in start..k {
            types[seq[j]] = d;
        }
    }
}

impl Paragraph {
    /// The levels of the entries `from..to` laid out as one line, with
    /// rule L1: segment and paragraph separators, and white space and
    /// isolate controls before them or at the end of the line, go back
    /// to the paragraph level.
    pub fn line_levels(&self, from: usize, to: usize) -> Vec<u8> {
        let mut out: Vec<u8> = self.levels[from..to].to_vec();
        let is_trailing = |c: u8| matches!(c, WS | FSI | LRI | RLI | PDI) || is_removed(c);
        let reset_before = |out: &mut Vec<u8>, end: usize| {
            let mut j = end;
            while j > from && is_trailing(self.classes[j - 1]) {
                j -= 1;
                out[j - from] = self.level;
            }
        };
        reset_before(&mut out, to);
        for i in from..to {
            if matches!(self.classes[i], S | B) {
                out[i - from] = self.level;
                reset_before(&mut out, i);
            }
        }
        out
    }
}

/// L2: the visual order of entries with `levels` (indices into
/// `levels`, left to right).
pub fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let Some(&max) = levels.iter().max() else {
        return order;
    };
    let min_odd = levels.iter().copied().filter(|l| l % 2 == 1).min();
    let Some(min_odd) = min_odd else {
        return order;
    };
    let mut lv = max;
    while lv >= min_odd {
        let mut k = 0;
        while k < order.len() {
            if levels[order[k]] >= lv {
                let start = k;
                while k < order.len() && levels[order[k]] >= lv {
                    k += 1;
                }
                order[start..k].reverse();
            } else {
                k += 1;
            }
        }
        if lv == 0 {
            break;
        }
        lv -= 1;
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels_of(s: &str, base: Option<u8>) -> Vec<u8> {
        let chars: Vec<u32> = s.chars().map(|c| c as u32).collect();
        let classes: Vec<u8> = chars.iter().map(|&c| class_of(c)).collect();
        resolve(&classes, &chars, base).levels
    }

    #[test]
    fn hebrew_inside_latin() {
        // "ab אב cd": the Hebrew letters are at level 1.
        assert_eq!(
            levels_of("ab \u{5D0}\u{5D1} cd", Some(0)),
            vec![0, 0, 0, 1, 1, 0, 0, 0]
        );
        // In a right-to-left paragraph, Latin letters are at level 2.
        assert_eq!(levels_of("\u{5D0} ab", None), vec![1, 1, 2, 2]);
    }

    #[test]
    fn numbers_and_brackets() {
        // European numbers after Hebrew stay at level 2.
        assert_eq!(levels_of("\u{5D0} 12", Some(1)), vec![1, 1, 2, 2]);
        // Brackets around Hebrew in a Latin paragraph take the
        // embedding direction only if their content says so.
        assert_eq!(
            levels_of("a (\u{5D0}) b", Some(0)),
            vec![0, 0, 0, 1, 0, 0, 0]
        );
    }

    #[test]
    fn reordering() {
        assert_eq!(visual_order(&[0, 0, 1, 1, 0]), vec![0, 1, 3, 2, 4]);
        assert_eq!(visual_order(&[1, 1, 2, 2]), vec![2, 3, 1, 0]);
        assert_eq!(mirror('(' as u32), Some(')' as u32));
    }
}
