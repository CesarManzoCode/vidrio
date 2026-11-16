//! The regular expression engine of vjs (ECMA-262 §22.2): the pattern
//! parser with the Annex B grammar, the `u` and `v` modes, named groups
//! (duplicates in different alternatives included), modifiers, lookbehind
//! and `\p{…}` with the Unicode 17.0.0 tables of [`crate::unicode`]; a
//! compiler to backtracking bytecode; and a matcher with an explicit stack.
//!
//! The `RegExp` built-ins are in [`crate::builtins::regexp`].

pub mod charset;
pub mod compiler;
pub mod parser;
pub mod vm;

pub use vm::StackExhausted;

/// The flags of a regular expression (`dgimsuvy`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub has_indices: bool,
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub unicode: bool,
    pub unicode_sets: bool,
    pub sticky: bool,
}

impl Flags {
    /// The flags of a flags string; `None` for an unknown or repeated
    /// flag, or for `u` together with `v`.
    pub fn parse(units: &[u16]) -> Option<Flags> {
        let mut f = Flags::default();
        for &u in units {
            let slot = match char::from_u32(u32::from(u))? {
                'd' => &mut f.has_indices,
                'g' => &mut f.global,
                'i' => &mut f.ignore_case,
                'm' => &mut f.multiline,
                's' => &mut f.dot_all,
                'u' => &mut f.unicode,
                'v' => &mut f.unicode_sets,
                'y' => &mut f.sticky,
                _ => return None,
            };
            if *slot {
                return None;
            }
            *slot = true;
        }
        if f.unicode && f.unicode_sets {
            return None;
        }
        Some(f)
    }

    /// `u` or `v`: characters are code points.
    pub fn either_unicode(self) -> bool {
        self.unicode || self.unicode_sets
    }
}

/// A compiled regular expression (`[[RegExpMatcher]]` and what
/// RegExpBuiltinExec needs to know about the groups).
pub struct Regex {
    program: compiler::Program,
    pub flags: Flags,
    /// The number of capturing groups.
    pub group_count: usize,
    /// The name of each group `1..=group_count` (index `i - 1`), as UTF-16.
    pub group_names: Vec<Option<Vec<u16>>>,
}

/// A successful match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// The `lastIndex` the match was found at (the `index` of the result).
    pub index: usize,
    /// The end of the match.
    pub end: usize,
    /// The groups `1..=group_count` (index `i - 1`).
    pub captures: Vec<Option<(usize, usize)>>,
}

/// The characters of a pattern: code points with `u` or `v`, code units
/// without.
fn pattern_chars(pattern: &[u16], unicode: bool) -> Vec<u32> {
    if unicode {
        crate::unicode::code_points(pattern)
    } else {
        pattern.iter().map(|&u| u32::from(u)).collect()
    }
}

fn is_lead(u: u16) -> bool {
    (0xD800..=0xDBFF).contains(&u)
}

fn is_trail(u: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&u)
}

/// AdvanceStringIndex(S, index, unicode).
pub fn advance_string_index(s: &[u16], index: usize, unicode: bool) -> usize {
    if !unicode || index + 1 >= s.len() {
        return index + 1;
    }
    if is_lead(s[index]) && is_trail(s[index + 1]) {
        index + 2
    } else {
        index + 1
    }
}

impl Regex {
    /// Parses and compiles `pattern`; the error is the SyntaxError message.
    pub fn new(pattern: &[u16], flags: Flags) -> Result<Regex, String> {
        let unicode = flags.either_unicode();
        let chars = pattern_chars(pattern, unicode);
        let parsed = parser::parse(&chars, flags)?;
        let program = compiler::compile(&parsed, unicode);
        let group_names = parsed
            .group_names
            .iter()
            .map(|n| n.as_ref().map(|cps| crate::unicode::to_utf16(cps)))
            .collect();
        Ok(Regex {
            program,
            flags,
            group_count: parsed.group_count as usize,
            group_names,
        })
    }

    /// Whether any group has a name.
    pub fn has_named_groups(&self) -> bool {
        self.group_names.iter().any(Option::is_some)
    }

    /// The matching loop of RegExpBuiltinExec from `last_index`: tries each
    /// position (only `last_index` when `sticky`) until one matches.
    pub fn exec(
        &self,
        input: &[u16],
        last_index: usize,
        sticky: bool,
    ) -> Result<Option<Match>, StackExhausted> {
        let unicode = self.flags.either_unicode();
        let mut vm = vm::Vm::new(&self.program, input);
        let mut last_index = last_index;
        loop {
            if last_index > input.len() {
                return Ok(None);
            }
            if !sticky && let Some(first) = self.program.first_unit {
                match input[last_index..].iter().position(|&u| u == first) {
                    Some(offset) => last_index += offset,
                    None => return Ok(None),
                }
            }
            // The index into the input of the character that holds the
            // code unit at lastIndex.
            let start = if unicode
                && last_index > 0
                && last_index < input.len()
                && is_trail(input[last_index])
                && is_lead(input[last_index - 1])
            {
                last_index - 1
            } else {
                last_index
            };
            if vm.run(start)? {
                let captures = (1..=self.group_count)
                    .map(|i| {
                        let (s, e) = (vm.caps[2 * i], vm.caps[2 * i + 1]);
                        (s != vm::UNSET && e != vm::UNSET).then_some((s, e))
                    })
                    .collect();
                return Ok(Some(Match {
                    index: last_index,
                    end: vm.caps[1],
                    captures,
                }));
            }
            if sticky {
                return Ok(None);
            }
            last_index = advance_string_index(input, last_index, unicode);
        }
    }

    /// Convenience for tests and the host: the first match at or after
    /// `start` without the `y` flag.
    pub fn find(&self, input: &[u16], start: usize) -> Result<Option<Match>, StackExhausted> {
        self.exec(input, start, self.flags.sticky)
    }
}

/// Checks a pattern and its flags; the error is the SyntaxError message.
pub fn validate(pattern: &[u16], flags: &[u16]) -> Result<(), String> {
    let Some(f) = Flags::parse(flags) else {
        return Err("invalid regular expression flags".into());
    };
    let chars = pattern_chars(pattern, f.either_unicode());
    parser::parse(&chars, f).map(|_| ())
}

/// The early errors of a RegularExpressionLiteral (§13.2.7.2): the
/// [`crate::parser::RegExpValidator`] of the engine.
pub fn validate_literal(pattern: &str, flags: &str) -> Result<(), String> {
    validate(&crate::string::utf16(pattern), &crate::string::utf16(flags))
}

/// Parse options with this engine's RegExp validator.
pub fn parse_options() -> crate::parser::ParseOptions {
    crate::parser::ParseOptions {
        regexp_validator: Some(validate_literal),
        ..crate::parser::ParseOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::string::utf16;

    fn find(p: &str, f: &str, s: &str) -> Option<(usize, usize)> {
        let re = Regex::new(&utf16(p), Flags::parse(&utf16(f)).unwrap()).unwrap();
        re.exec(&utf16(s), 0, false)
            .unwrap()
            .map(|m| (m.index, m.end))
    }

    #[test]
    fn basics() {
        assert_eq!(find("b+", "", "abbbc"), Some((1, 4)));
        assert_eq!(find("b+?", "", "abbbc"), Some((1, 2)));
        assert_eq!(find("(a|ab)(c|bcd)(d*)", "", "abcd"), Some((0, 4)));
        assert_eq!(find("x", "", "abc"), None);
        assert_eq!(find("^b", "m", "a\nb"), Some((2, 3)));
        assert_eq!(find("(?<=\\$)\\d+", "", "cost $42"), Some((6, 8)));
        assert_eq!(find("\u{1F600}", "u", "a\u{1F600}"), Some((1, 3)));
        assert_eq!(find("^.$", "u", "\u{1F600}"), Some((0, 2)));
        assert_eq!(find("^.$", "", "\u{1F600}"), None);
    }

    #[test]
    fn flags() {
        assert!(Flags::parse(&utf16("dgimsuy")).is_some());
        assert!(Flags::parse(&utf16("gg")).is_none());
        assert!(Flags::parse(&utf16("uv")).is_none());
        assert!(Flags::parse(&utf16("x")).is_none());
    }
}
