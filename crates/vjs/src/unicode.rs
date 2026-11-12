//! Code point classes of the lexical grammar (ECMA-262 §12): identifiers
//! (Unicode 17.0.0 ID_Start/ID_Continue), white space and line terminators.

use crate::generated::unicode_id::{ID_CONTINUE, ID_START};

fn in_table(table: &[(u32, u32)], c: char) -> bool {
    let c = c as u32;
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
}
