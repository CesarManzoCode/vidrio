//! Line breaking opportunities: the rules LB1 to LB31 of UAX #14 over the
//! Line_Break classes of Unicode 17.0.0 (`generated/linebreak.rs`, from
//! `crates/vidrio/gen/linebreak.py`), tailored by css-text-3 §5:
//! `word-break` (`break-all`, `keep-all`), `line-break` (`strict` keeps
//! CJ as NS, `anywhere` breaks between typographic character units) and
//! the soft wrap opportunities around atomic inlines.

use crate::generated::linebreak::*;

/// The opportunity before a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Break {
    None,
    Allowed,
    Mandatory,
}

/// `word-break`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WordBreak {
    #[default]
    Normal,
    BreakAll,
    KeepAll,
}

/// `line-break`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Strictness {
    Loose,
    #[default]
    Normal,
    Strict,
    Anywhere,
}

/// The CSS properties that tailor breaking, per character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Options {
    pub word_break: WordBreak,
    pub strictness: Strictness,
}

fn in_ranges(table: &[(u32, u32)], c: u32) -> bool {
    table
        .binary_search_by(|&(a, b)| {
            if b < c {
                std::cmp::Ordering::Less
            } else if a > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// The Line_Break class of `c`.
pub fn class_of(c: u32) -> u8 {
    match LINE_BREAK.binary_search_by(|&(a, b, _)| {
        if b < c {
            std::cmp::Ordering::Less
        } else if a > c {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => LINE_BREAK[i].2,
        Err(_) => LB_XX,
    }
}

/// East_Asian_Width is F, W or H.
pub fn is_east_asian_wide(c: u32) -> bool {
    in_ranges(EAST_ASIAN_WIDE, c)
}

pub fn is_extended_pictographic(c: u32) -> bool {
    in_ranges(EXTENDED_PICTOGRAPHIC, c)
}

/// Characters that extend the grapheme before them (an approximation of
/// UAX #29 Grapheme_Extend plus joiners and spacing marks).
pub fn is_grapheme_extend(c: char) -> bool {
    let cp = c as u32;
    matches!(cp, 0x200C | 0x200D | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF | 0x1F3FB..=0x1F3FF)
        || matches!(vjs::unicode::general_category(cp), "Mn" | "Me" | "Mc")
}

/// LB1 and the css-text tailorings: the class each character is treated
/// as.
fn resolve(c: char, o: Options) -> u8 {
    let cp = c as u32;
    let mut cls = class_of(cp);
    cls = match cls {
        LB_AI | LB_SG | LB_XX => LB_AL,
        LB_SA => {
            if matches!(vjs::unicode::general_category(cp), "Mn" | "Mc") {
                LB_CM
            } else {
                LB_AL
            }
        }
        LB_CJ => {
            if o.strictness == Strictness::Strict {
                LB_NS
            } else {
                LB_ID
            }
        }
        x => x,
    };
    match o.word_break {
        WordBreak::BreakAll => {
            if matches!(cls, LB_AL | LB_HL | LB_NU | LB_AK | LB_AS | LB_AP) {
                cls = LB_ID;
            }
        }
        WordBreak::KeepAll => {
            if matches!(cls, LB_ID | LB_H2 | LB_H3 | LB_JL | LB_JV | LB_JT)
                && vjs::unicode::general_category(cp).starts_with('L')
            {
                cls = LB_AL;
            }
        }
        WordBreak::Normal => {}
    }
    cls
}

/// The break opportunities of `chars`: element `i` is the opportunity
/// between `chars[i - 1]` and `chars[i]` (element 0 is always `None`;
/// the end of the text is a mandatory break and is not listed).
/// `opts[i]` are the properties of `chars[i]`. U+FFFC stands for an
/// atomic inline: there is an opportunity on both sides of it.
pub fn opportunities(chars: &[char], opts: &[Options]) -> Vec<Break> {
    let n = chars.len();
    let mut out = vec![Break::None; n];
    if n == 0 {
        return out;
    }
    let opt = |i: usize| opts.get(i).copied().unwrap_or_default();
    let cls: Vec<u8> = (0..n).map(|i| resolve(chars[i], opt(i))).collect();
    // LB9 and LB10: combining marks take the class of their base.
    let mut eff = cls.clone();
    let mut attached = vec![false; n];
    for i in 0..n {
        if cls[i] == LB_CM || cls[i] == LB_ZWJ {
            let base_ok =
                i > 0 && !matches!(eff[i - 1], LB_BK | LB_CR | LB_LF | LB_NL | LB_SP | LB_ZW);
            if base_ok {
                eff[i] = eff[i - 1];
                attached[i] = true;
            } else {
                eff[i] = LB_AL;
            }
        }
    }
    let gc = |i: usize| vjs::unicode::general_category(chars[i] as u32);
    let ea = |i: usize| is_east_asian_wide(chars[i] as u32);
    // The class before position `i` once spaces are skipped.
    let before_spaces = |i: usize| -> Option<usize> {
        let mut k = i;
        while k > 0 {
            k -= 1;
            if eff[k] != LB_SP {
                return Some(k);
            }
        }
        None
    };
    let dotted = |i: usize| chars[i] == '\u{25CC}';
    let aksara = |i: usize| matches!(eff[i], LB_AK | LB_AS) || dotted(i);
    for i in 1..n {
        let a = eff[i - 1];
        let b = eff[i];
        let raw_b = cls[i];
        let decide = || -> Break {
            // LB4, LB5
            if a == LB_BK {
                return Break::Mandatory;
            }
            if a == LB_CR && raw_b == LB_LF {
                return Break::None;
            }
            if matches!(a, LB_CR | LB_LF | LB_NL) {
                return Break::Mandatory;
            }
            // LB6, LB7
            if matches!(raw_b, LB_BK | LB_CR | LB_LF | LB_NL) {
                return Break::None;
            }
            if matches!(raw_b, LB_SP | LB_ZW) {
                return Break::None;
            }
            // LB8
            if let Some(k) = before_spaces(i) {
                if eff[k] == LB_ZW && (a == LB_ZW || a == LB_SP) {
                    return Break::Allowed;
                }
            }
            // LB8a
            if cls[i - 1] == LB_ZWJ {
                return Break::None;
            }
            // LB9
            if attached[i] {
                return Break::None;
            }
            // The atomic inline replacement character.
            if chars[i] == '\u{FFFC}' || chars[i - 1] == '\u{FFFC}' {
                return Break::Allowed;
            }
            // LB11
            if a == LB_WJ || b == LB_WJ {
                return Break::None;
            }
            // LB12, LB12a
            if a == LB_GL {
                return Break::None;
            }
            if b == LB_GL && !matches!(a, LB_SP | LB_BA | LB_HY | LB_HH) {
                return Break::None;
            }
            // LB13
            if matches!(b, LB_CL | LB_CP | LB_EX | LB_SY) {
                return Break::None;
            }
            // LB14
            if let Some(k) = before_spaces(i) {
                if eff[k] == LB_OP {
                    return Break::None;
                }
            }
            // LB15a
            if let Some(k) = before_spaces(i) {
                if eff[k] == LB_QU
                    && gc(k) == "Pi"
                    && (k == 0
                        || matches!(
                            eff[k - 1],
                            LB_BK | LB_CR | LB_LF | LB_NL | LB_OP | LB_QU | LB_GL | LB_SP | LB_ZW
                        ))
                {
                    return Break::None;
                }
            }
            // LB15b
            if b == LB_QU
                && gc(i) == "Pf"
                && (i + 1 == n
                    || matches!(
                        eff[i + 1],
                        LB_SP
                            | LB_GL
                            | LB_WJ
                            | LB_CL
                            | LB_QU
                            | LB_CP
                            | LB_EX
                            | LB_IS
                            | LB_SY
                            | LB_BK
                            | LB_CR
                            | LB_LF
                            | LB_NL
                            | LB_ZW
                    ))
            {
                return Break::None;
            }
            // LB15c, LB15d
            if a == LB_SP && b == LB_IS && i + 1 < n && eff[i + 1] == LB_NU {
                return Break::Allowed;
            }
            if b == LB_IS {
                return Break::None;
            }
            // LB16
            if b == LB_NS {
                if let Some(k) = before_spaces(i) {
                    if matches!(eff[k], LB_CL | LB_CP) {
                        return Break::None;
                    }
                }
            }
            // LB17
            if b == LB_B2 {
                if let Some(k) = before_spaces(i) {
                    if eff[k] == LB_B2 {
                        return Break::None;
                    }
                }
            }
            // LB18
            if a == LB_SP {
                return Break::Allowed;
            }
            // LB19
            if b == LB_QU && gc(i) != "Pi" {
                return Break::None;
            }
            if a == LB_QU && gc(i - 1) != "Pf" {
                return Break::None;
            }
            // LB19a
            if b == LB_QU && (!ea(i - 1) || i + 1 == n || !ea(i + 1)) {
                return Break::None;
            }
            if a == LB_QU && (!ea(i) || i < 2 || !ea(i - 2)) {
                return Break::None;
            }
            // LB20
            if a == LB_CB || b == LB_CB {
                return Break::Allowed;
            }
            // LB20a
            if matches!(a, LB_HY | LB_HH)
                && matches!(b, LB_AL | LB_HL)
                && (i < 2
                    || matches!(
                        eff[i - 2],
                        LB_BK | LB_CR | LB_LF | LB_NL | LB_SP | LB_ZW | LB_CB | LB_GL
                    ))
            {
                return Break::None;
            }
            // LB21
            if matches!(b, LB_BA | LB_HY | LB_HH | LB_NS) || a == LB_BB {
                return Break::None;
            }
            // LB21a
            if i >= 2
                && eff[i - 2] == LB_HL
                && (matches!(a, LB_HY | LB_HH) || (a == LB_BA && !ea(i - 1)))
                && b != LB_HL
            {
                return Break::None;
            }
            // LB21b
            if a == LB_SY && b == LB_HL {
                return Break::None;
            }
            // LB22
            if b == LB_IN {
                return Break::None;
            }
            // LB23, LB23a
            if (matches!(a, LB_AL | LB_HL) && b == LB_NU)
                || (a == LB_NU && matches!(b, LB_AL | LB_HL))
            {
                return Break::None;
            }
            if (a == LB_PR && matches!(b, LB_ID | LB_EB | LB_EM))
                || (matches!(a, LB_ID | LB_EB | LB_EM) && b == LB_PO)
            {
                return Break::None;
            }
            // LB24
            if (matches!(a, LB_PR | LB_PO) && matches!(b, LB_AL | LB_HL))
                || (matches!(a, LB_AL | LB_HL) && matches!(b, LB_PR | LB_PO))
            {
                return Break::None;
            }
            // LB25 (the numeric sequences of the rule's regular expressions)
            if matches!(a, LB_PR | LB_PO) && b == LB_NU {
                return Break::None;
            }
            if matches!(a, LB_PR | LB_PO)
                && matches!(b, LB_OP | LB_HY)
                && i + 1 < n
                && eff[i + 1] == LB_NU
            {
                return Break::None;
            }
            if matches!(a, LB_OP | LB_HY) && b == LB_NU {
                return Break::None;
            }
            let numeric_run_before = |end: usize| -> bool {
                // Is chars[..=end] ending in NU (NU | SY | IS)*?
                let mut k = end as isize;
                let mut start = None;
                while k >= 0 && matches!(eff[k as usize], LB_NU | LB_SY | LB_IS) {
                    if eff[k as usize] == LB_NU {
                        start = Some(k as usize);
                    }
                    k -= 1;
                }
                start.is_some()
            };
            if matches!(b, LB_NU | LB_SY | LB_IS | LB_CL | LB_CP) && numeric_run_before(i - 1) {
                return Break::None;
            }
            if matches!(b, LB_PO | LB_PR) {
                let mut end = i - 1;
                if matches!(eff[end], LB_CL | LB_CP) && end > 0 {
                    end -= 1;
                }
                if numeric_run_before(end) {
                    return Break::None;
                }
            }
            // LB26
            if (a == LB_JL && matches!(b, LB_JL | LB_JV | LB_H2 | LB_H3))
                || (matches!(a, LB_JV | LB_H2) && matches!(b, LB_JV | LB_JT))
                || (matches!(a, LB_JT | LB_H3) && b == LB_JT)
            {
                return Break::None;
            }
            // LB27
            if (matches!(a, LB_JL | LB_JV | LB_JT | LB_H2 | LB_H3) && b == LB_PO)
                || (a == LB_PR && matches!(b, LB_JL | LB_JV | LB_JT | LB_H2 | LB_H3))
            {
                return Break::None;
            }
            // LB28
            if matches!(a, LB_AL | LB_HL) && matches!(b, LB_AL | LB_HL) {
                return Break::None;
            }
            // LB28a
            if a == LB_AP && aksara(i) {
                return Break::None;
            }
            if aksara(i - 1) && matches!(b, LB_VF | LB_VI) {
                return Break::None;
            }
            if i >= 2 && aksara(i - 2) && a == LB_VI && (b == LB_AK || dotted(i)) {
                return Break::None;
            }
            if aksara(i - 1) && aksara(i) && i + 1 < n && eff[i + 1] == LB_VF {
                return Break::None;
            }
            // LB29
            if a == LB_IS && matches!(b, LB_AL | LB_HL) {
                return Break::None;
            }
            // LB30
            if matches!(a, LB_AL | LB_HL | LB_NU) && b == LB_OP && !ea(i) {
                return Break::None;
            }
            if a == LB_CP && !ea(i - 1) && matches!(b, LB_AL | LB_HL | LB_NU) {
                return Break::None;
            }
            // LB30a
            if a == LB_RI && b == LB_RI {
                let mut count = 0;
                let mut k = i;
                while k > 0 && eff[k - 1] == LB_RI {
                    count += 1;
                    k -= 1;
                }
                if count % 2 == 1 {
                    return Break::None;
                }
            }
            // LB30b
            if b == LB_EM
                && (a == LB_EB
                    || (is_extended_pictographic(chars[i - 1] as u32) && gc(i - 1) == "Cn"))
            {
                return Break::None;
            }
            // LB31
            Break::Allowed
        };
        let mut r = decide();
        // `line-break: anywhere`: an opportunity between typographic
        // character units, mandatory breaks kept.
        if r == Break::None
            && (opt(i).strictness == Strictness::Anywhere
                || opt(i - 1).strictness == Strictness::Anywhere)
            && !attached[i]
            && !is_grapheme_extend(chars[i])
            && cls[i - 1] != LB_ZWJ
            && !(a == LB_CR && raw_b == LB_LF)
        {
            r = Break::Allowed;
        }
        // `line-break: loose` also allows breaks before small kana and
        // the other CJ characters, and before iteration marks.
        if r == Break::None
            && opt(i).strictness == Strictness::Loose
            && (cls[i] == LB_CJ
                || matches!(
                    chars[i],
                    '\u{3005}' | '\u{303B}' | '\u{309D}' | '\u{309E}' | '\u{30FD}' | '\u{30FE}'
                ))
            && !matches!(a, LB_OP | LB_SP)
        {
            r = Break::Allowed;
        }
        out[i] = r;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breaks(s: &str) -> Vec<usize> {
        let chars: Vec<char> = s.chars().collect();
        let opts = vec![Options::default(); chars.len()];
        opportunities(&chars, &opts)
            .iter()
            .enumerate()
            .filter(|(_, b)| **b != Break::None)
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn spaces_hyphens_and_punctuation() {
        assert_eq!(breaks("ab cd"), vec![3]);
        assert_eq!(breaks("ab  cd"), vec![4]);
        assert_eq!(breaks("well-known"), vec![5]);
        assert_eq!(breaks("(ab) cd."), vec![5]);
        assert_eq!(breaks("a\u{A0}b"), Vec::<usize>::new());
        assert_eq!(breaks("12,345.6 x"), vec![9]);
        assert_eq!(breaks("日本語"), vec![1, 2]);
        assert_eq!(breaks("soft\u{AD}hyphen"), vec![5]);
    }

    #[test]
    fn mandatory_breaks() {
        let chars: Vec<char> = "a\nb".chars().collect();
        let opts = vec![Options::default(); 3];
        assert_eq!(opportunities(&chars, &opts)[2], Break::Mandatory);
    }

    #[test]
    fn tailorings() {
        let chars: Vec<char> = "abc".chars().collect();
        let all = vec![
            Options {
                word_break: WordBreak::BreakAll,
                ..Options::default()
            };
            3
        ];
        assert_eq!(
            opportunities(&chars, &all),
            vec![Break::None, Break::Allowed, Break::Allowed]
        );
        let anywhere = vec![
            Options {
                strictness: Strictness::Anywhere,
                ..Options::default()
            };
            3
        ];
        assert_eq!(opportunities(&chars, &anywhere)[1], Break::Allowed);
        let cjk: Vec<char> = "日本".chars().collect();
        let keep = vec![
            Options {
                word_break: WordBreak::KeepAll,
                ..Options::default()
            };
            2
        ];
        assert_eq!(opportunities(&cjk, &keep)[1], Break::None);
    }
}
