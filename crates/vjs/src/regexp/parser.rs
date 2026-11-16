//! Pattern parser (ECMA-262 §22.2.1 with the Annex B grammar of B.1.2):
//! turns the characters of a pattern into a [`Node`] tree and reports
//! every early error.
//!
//! The pattern arrives as characters: code points with the `u` or `v`
//! flag, code units without. Flags that the modifiers `(?ims-ims:…)` can
//! change are tracked while parsing, so every node already carries the
//! `IgnoreCase`, `Multiline` and `DotAll` that apply to it, and character
//! sets come out with their case closure applied.

use super::Flags;
use super::charset::{
    self, CaseFold, CharSet, MAX_CODE_POINT, MAX_CODE_UNIT, canonical_image, canonicalize,
};
use crate::unicode::{self, PropertyMatch};

type R<T> = Result<T, String>;

/// Nesting deeper than this (groups, lookarounds, nested `v` classes) is a
/// SyntaxError, so that the recursive parser and compiler stay within the
/// native stack.
pub const MAX_NESTING: usize = 1000;

/// An infinite quantifier bound.
pub const INFINITE: u64 = u64::MAX;

#[derive(Clone, Debug)]
pub enum Node {
    Empty,
    /// A character; with `ci` it is already canonicalized.
    Char {
        c: u32,
        ci: bool,
    },
    /// A character set; with `ci` it is already the canonical image.
    Set {
        set: CharSet,
        invert: bool,
        ci: bool,
    },
    /// A `v` class that contains strings: the strings of two or more
    /// characters (longest first), the single characters, and whether the
    /// empty string is a member. With `ci` everything is case folded.
    Strings {
        strings: Vec<Vec<u32>>,
        set: CharSet,
        empty: bool,
        ci: bool,
    },
    Dot {
        dot_all: bool,
    },
    LineStart {
        multiline: bool,
    },
    LineEnd {
        multiline: bool,
    },
    WordBoundary {
        negate: bool,
        unicode_ci: bool,
    },
    /// A backreference to any of the groups (several with duplicate names).
    BackRef {
        groups: Vec<u32>,
        ci: bool,
    },
    NamedBackRef {
        name: Vec<u32>,
        ci: bool,
    },
    Group {
        index: u32,
        node: Box<Node>,
    },
    Look {
        behind: bool,
        negate: bool,
        node: Box<Node>,
    },
    Repeat {
        node: Box<Node>,
        min: u64,
        max: u64,
        greedy: bool,
        /// The capturing groups inside: `first_group..first_group + group_count`.
        first_group: u32,
        group_count: u32,
    },
    Seq(Vec<Node>),
    Alt(Vec<Node>),
}

/// A parsed pattern.
pub struct Pattern {
    pub node: Node,
    pub group_count: u32,
    /// The name of each group `1..=group_count` (index `i - 1`).
    pub group_names: Vec<Option<Vec<u32>>>,
}

struct NamedGroup {
    name: Vec<u32>,
    index: u32,
    /// (Disjunction, alternative) pairs from the root to the group.
    path: Vec<(u32, u32)>,
}

/// A `v` mode class value: single characters and strings of any other
/// length, plus whether the syntax MayContainStrings.
#[derive(Clone, Debug, Default)]
struct ClassValue {
    set: CharSet,
    strings: Vec<Vec<u32>>,
    may_strings: bool,
}

impl ClassValue {
    fn chars(set: CharSet) -> ClassValue {
        ClassValue {
            set,
            strings: Vec::new(),
            may_strings: false,
        }
    }

    fn union(mut self, other: ClassValue) -> ClassValue {
        self.set = self.set.union(&other.set);
        self.strings.extend(other.strings);
        self.strings.sort();
        self.strings.dedup();
        self.may_strings |= other.may_strings;
        self
    }

    fn intersect(mut self, other: &ClassValue) -> ClassValue {
        self.set = self.set.intersect(&other.set);
        self.strings.retain(|s| other.strings.contains(s));
        self
    }

    fn subtract(mut self, other: &ClassValue) -> ClassValue {
        self.set = self.set.subtract(&other.set);
        self.strings.retain(|s| !other.strings.contains(s));
        self
    }
}

/// A ClassAtom of a legacy or `u` class.
enum ClassAtom {
    Char(u32),
    Set(CharSet),
}

/// A ClassSetOperand of a `v` class.
enum Operand {
    Char(u32),
    Class(ClassValue),
}

const fn ch(c: char) -> u32 {
    c as u32
}

fn is_syntax_char(c: u32) -> bool {
    matches!(
        char::from_u32(c),
        Some('^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|')
    )
}

fn is_lead(c: u32) -> bool {
    (0xD800..=0xDBFF).contains(&c)
}

fn is_trail(c: u32) -> bool {
    (0xDC00..=0xDFFF).contains(&c)
}

fn hex_value(c: u32) -> Option<u32> {
    char::from_u32(c).and_then(|c| c.to_digit(16))
}

fn is_digit(c: Option<u32>) -> bool {
    matches!(c, Some(c) if (ch('0')..=ch('9')).contains(&c))
}

fn is_octal(c: Option<u32>) -> bool {
    matches!(c, Some(c) if (ch('0')..=ch('7')).contains(&c))
}

fn is_ascii_letter(c: u32) -> bool {
    c < 128 && (c as u8).is_ascii_alphabetic()
}

/// Counts the capturing groups of a pattern and tells whether any has a
/// name, without parsing it (CountLeftCapturingParensWithin, and the
/// Annex B choice of `[+NamedCaptureGroups]`).
fn prescan(src: &[u32], sets: bool) -> (u32, bool) {
    let (mut count, mut named) = (0u32, false);
    let mut class_depth = 0usize;
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if c == ch('\\') {
            i += 2;
            continue;
        }
        if class_depth > 0 {
            if c == ch(']') {
                class_depth -= 1;
            } else if c == ch('[') && sets {
                class_depth += 1;
            }
        } else if c == ch('[') {
            class_depth = 1;
        } else if c == ch('(') {
            if src.get(i + 1) == Some(&ch('?')) {
                if src.get(i + 2) == Some(&ch('<'))
                    && !matches!(src.get(i + 3), Some(&c) if c == ch('=') || c == ch('!'))
                {
                    count = count.saturating_add(1);
                    named = true;
                }
            } else {
                count = count.saturating_add(1);
            }
        }
        i += 1;
    }
    (count, named)
}

/// Parses the characters of a pattern with the given flags.
pub fn parse(src: &[u32], flags: Flags) -> R<Pattern> {
    let unicode = flags.unicode || flags.unicode_sets;
    let (total_groups, has_names) = prescan(src, flags.unicode_sets);
    let mut p = Parser {
        src,
        pos: 0,
        unicode,
        sets: flags.unicode_sets,
        named: unicode || has_names,
        total_groups,
        groups: 0,
        names: Vec::new(),
        refs: Vec::new(),
        ci: flags.ignore_case,
        multiline: flags.multiline,
        dot_all: flags.dot_all,
        path: Vec::new(),
        disjunctions: 0,
        depth: 0,
        fold: CaseFold::for_unicode(unicode),
    };
    let node = p.parse_disjunction()?;
    if p.pos < src.len() {
        return Err(if src[p.pos] == ch(')') {
            "unmatched ')'".into()
        } else {
            "unexpected character".into()
        });
    }
    for name in &p.refs {
        if !p.names.iter().any(|g| &g.name == name) {
            return Err("invalid named capture referenced".into());
        }
    }
    let mut group_names = vec![None; p.groups as usize];
    for g in p.names {
        group_names[g.index as usize - 1] = Some(g.name);
    }
    Ok(Pattern {
        node,
        group_count: p.groups,
        group_names,
    })
}

struct Parser<'a> {
    src: &'a [u32],
    pos: usize,
    /// `u` or `v`.
    unicode: bool,
    /// `v`.
    sets: bool,
    /// `[+NamedCaptureGroups]`.
    named: bool,
    total_groups: u32,
    groups: u32,
    names: Vec<NamedGroup>,
    refs: Vec<Vec<u32>>,
    ci: bool,
    multiline: bool,
    dot_all: bool,
    path: Vec<(u32, u32)>,
    disjunctions: u32,
    depth: usize,
    fold: CaseFold,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u32> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, k: usize) -> Option<u32> {
        self.src.get(self.pos + k).copied()
    }

    fn at(&self, c: char) -> bool {
        self.peek() == Some(ch(c))
    }

    fn at2(&self, a: char, b: char) -> bool {
        self.peek() == Some(ch(a)) && self.peek_at(1) == Some(ch(b))
    }

    fn eat(&mut self, c: char) -> bool {
        if self.at(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char, message: &str) -> R<()> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn enter(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err("regular expression too deeply nested".into());
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // ----- nodes -----

    fn char_node(&self, c: u32) -> Node {
        if self.ci {
            Node::Char {
                c: canonicalize(c, self.fold),
                ci: true,
            }
        } else {
            Node::Char { c, ci: false }
        }
    }

    /// A set matcher. `folded` sets (from `v` mode) are already canonical.
    fn set_node(&self, set: CharSet, invert: bool, folded: bool) -> Node {
        let set = if self.ci && !folded {
            canonical_image(&set, self.fold)
        } else {
            set
        };
        Node::Set {
            set,
            invert,
            ci: self.ci,
        }
    }

    /// The matcher of a `v` mode class value.
    fn class_value_node(&self, value: ClassValue) -> Node {
        if value.strings.is_empty() {
            return self.set_node(value.set, false, true);
        }
        let empty = value.strings.iter().any(|s| s.is_empty());
        let mut strings: Vec<Vec<u32>> = value
            .strings
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        strings.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        Node::Strings {
            strings,
            set: value.set,
            empty,
            ci: self.ci,
        }
    }

    // ----- character sets -----

    /// AllCharacters(rer).
    fn all_chars(&self) -> CharSet {
        if self.sets && self.ci {
            charset::canonical_code_points().clone()
        } else if self.unicode {
            CharSet::range(0, MAX_CODE_POINT)
        } else {
            CharSet::range(0, MAX_CODE_UNIT)
        }
    }

    /// CharacterComplement(rer, set).
    fn complement(&self, set: &CharSet) -> CharSet {
        self.all_chars().subtract(set)
    }

    /// MaybeSimpleCaseFolding(rer, set).
    fn maybe_fold(&self, set: CharSet) -> CharSet {
        if self.sets && self.ci {
            canonical_image(&set, CaseFold::Unicode)
        } else {
            set
        }
    }

    fn maybe_fold_char(&self, c: u32) -> u32 {
        if self.sets && self.ci {
            canonicalize(c, CaseFold::Unicode)
        } else {
            c
        }
    }

    fn maybe_fold_string(&self, s: Vec<u32>) -> Vec<u32> {
        if self.sets && self.ci {
            s.into_iter()
                .map(|c| canonicalize(c, CaseFold::Unicode))
                .collect()
        } else {
            s
        }
    }

    /// `\d \D \s \S \w \W` (the letter is already consumed).
    fn class_escape_set(&self, letter: u32) -> CharSet {
        let lower = letter | 0x20;
        let base = if lower == ch('d') {
            charset::digit_chars()
        } else if lower == ch('s') {
            charset::space_chars()
        } else {
            self.maybe_fold(charset::word_chars(self.unicode && self.ci))
        };
        if letter == lower {
            base
        } else {
            self.complement(&base)
        }
    }

    /// `\p{…}` or `\P{…}` after the letter (UnicodePropertyValueExpression).
    fn property_escape(&mut self, negate: bool) -> R<ClassValue> {
        let invalid = || "invalid property name".to_string();
        if !self.eat('{') {
            return Err(invalid());
        }
        let mut text = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(invalid());
            };
            self.pos += 1;
            if c == ch('}') {
                break;
            }
            match char::from_u32(c) {
                Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '=' => text.push(c),
                _ => return Err(invalid()),
            }
        }
        let (name, value) = match text.split_once('=') {
            Some((n, v)) => (Some(n), v),
            None => (None, text.as_str()),
        };
        if let Some(n) = name
            && (n.is_empty() || !n.chars().all(|c| c.is_ascii_alphabetic() || c == '_'))
        {
            return Err(invalid());
        }
        if value.is_empty() || value.contains('=') {
            return Err(invalid());
        }
        match unicode::property_escape(name, value) {
            Some(PropertyMatch::CodePoints(ranges)) => {
                let set = self.maybe_fold(CharSet::from_ranges(ranges));
                Ok(ClassValue::chars(if negate {
                    self.complement(&set)
                } else {
                    set
                }))
            }
            Some(PropertyMatch::Strings(prop)) if self.sets && !negate => {
                let set = self.maybe_fold(CharSet::from_ranges(prop.code_points));
                let mut strings: Vec<Vec<u32>> = prop
                    .strings
                    .iter()
                    .map(|s| self.maybe_fold_string(s.to_vec()))
                    .collect();
                strings.sort();
                strings.dedup();
                Ok(ClassValue {
                    set,
                    strings,
                    may_strings: true,
                })
            }
            _ => Err(invalid()),
        }
    }

    // ----- disjunctions and terms -----

    fn parse_disjunction(&mut self) -> R<Node> {
        self.enter()?;
        let id = self.disjunctions;
        self.disjunctions += 1;
        let mut alternatives = Vec::new();
        let mut k = 0;
        loop {
            self.path.push((id, k));
            let alternative = self.parse_alternative();
            self.path.pop();
            alternatives.push(alternative?);
            if self.eat('|') {
                k += 1;
            } else {
                break;
            }
        }
        self.leave();
        Ok(if alternatives.len() == 1 {
            alternatives.pop().expect("one alternative")
        } else {
            Node::Alt(alternatives)
        })
    }

    fn parse_alternative(&mut self) -> R<Node> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == ch('|') || c == ch(')') {
                break;
            }
            self.parse_term(&mut items)?;
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().expect("one term"),
            _ => Node::Seq(items),
        })
    }

    fn parse_term(&mut self, items: &mut Vec<Node>) -> R<()> {
        let c = self.peek().expect("a term");
        if c == ch('^') {
            self.pos += 1;
            items.push(Node::LineStart {
                multiline: self.multiline,
            });
            return Ok(());
        }
        if c == ch('$') {
            self.pos += 1;
            items.push(Node::LineEnd {
                multiline: self.multiline,
            });
            return Ok(());
        }
        if c == ch('\\') && matches!(self.peek_at(1), Some(b) if b == ch('b') || b == ch('B')) {
            let negate = self.peek_at(1) == Some(ch('B'));
            self.pos += 2;
            items.push(Node::WordBoundary {
                negate,
                unicode_ci: self.unicode && self.ci,
            });
            return Ok(());
        }
        if c == ch('(') && self.peek_at(1) == Some(ch('?')) {
            let third = self.peek_at(2);
            let lookahead = matches!(third, Some(t) if t == ch('=') || t == ch('!'));
            let lookbehind = third == Some(ch('<'))
                && matches!(self.peek_at(3), Some(t) if t == ch('=') || t == ch('!'));
            if lookahead || lookbehind {
                let negate = if lookahead {
                    third == Some(ch('!'))
                } else {
                    self.peek_at(3) == Some(ch('!'))
                };
                self.pos += if lookahead { 3 } else { 4 };
                let groups_before = self.groups;
                let node = self.parse_disjunction()?;
                self.expect(')', "unterminated group")?;
                let look = Node::Look {
                    behind: lookbehind,
                    negate,
                    node: Box::new(node),
                };
                // Annex B: QuantifiableAssertion (lookaheads without u/v).
                if lookahead && !self.unicode {
                    return self.parse_quantifier(items, look, groups_before);
                }
                items.push(look);
                return Ok(());
            }
        }
        let groups_before = self.groups;
        let atom = self.parse_atom()?;
        self.parse_quantifier(items, atom, groups_before)
    }

    /// `{n}`, `{n,}` or `{n,m}` at `self.pos`: the bounds, whether they
    /// are out of order, and the length of the text.
    fn braced_quantifier(&self) -> Option<(u64, u64, bool, usize)> {
        let src = self.src;
        let mut i = self.pos;
        if src.get(i) != Some(&ch('{')) {
            return None;
        }
        i += 1;
        let digits = |i: &mut usize| -> Option<(u64, Vec<u32>)> {
            let start = *i;
            let mut value = 0u64;
            while is_digit(src.get(*i).copied()) {
                value = value
                    .saturating_mul(10)
                    .saturating_add(u64::from(src[*i] - ch('0')));
                *i += 1;
            }
            if *i == start {
                return None;
            }
            let mut text: Vec<u32> = src[start..*i].to_vec();
            while text.len() > 1 && text[0] == ch('0') {
                text.remove(0);
            }
            Some((value, text))
        };
        let (min, min_text) = digits(&mut i)?;
        let (max, out_of_order) = if src.get(i) == Some(&ch(',')) {
            i += 1;
            if src.get(i) == Some(&ch('}')) {
                (INFINITE, false)
            } else {
                let (max, max_text) = digits(&mut i)?;
                let out = min_text.len() > max_text.len()
                    || (min_text.len() == max_text.len() && min_text > max_text);
                (max, out)
            }
        } else {
            (min, false)
        };
        if src.get(i) != Some(&ch('}')) {
            return None;
        }
        Some((min, max, out_of_order, i + 1 - self.pos))
    }

    fn parse_quantifier(&mut self, items: &mut Vec<Node>, atom: Node, groups_before: u32) -> R<()> {
        let bounds = match self.peek() {
            Some(c) if c == ch('*') => {
                self.pos += 1;
                Some((0, INFINITE))
            }
            Some(c) if c == ch('+') => {
                self.pos += 1;
                Some((1, INFINITE))
            }
            Some(c) if c == ch('?') => {
                self.pos += 1;
                Some((0, 1))
            }
            Some(c) if c == ch('{') => match self.braced_quantifier() {
                Some((min, max, out_of_order, len)) => {
                    if out_of_order {
                        return Err("numbers out of order in {} quantifier".into());
                    }
                    self.pos += len;
                    Some((min, max))
                }
                None if self.unicode => return Err("incomplete quantifier".into()),
                None => None,
            },
            _ => None,
        };
        let Some((min, max)) = bounds else {
            items.push(atom);
            return Ok(());
        };
        let greedy = !self.eat('?');
        items.push(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
            first_group: groups_before + 1,
            group_count: self.groups - groups_before,
        });
        Ok(())
    }

    fn parse_atom(&mut self) -> R<Node> {
        let c = self.peek().expect("an atom");
        match char::from_u32(c) {
            Some('.') => {
                self.pos += 1;
                Ok(Node::Dot {
                    dot_all: self.dot_all,
                })
            }
            Some('(') => self.parse_group(),
            Some('[') => {
                self.pos += 1;
                self.parse_class()
            }
            Some('\\') => self.parse_atom_escape(),
            Some('*' | '+' | '?') => Err("nothing to repeat".into()),
            Some('{') => {
                if self.unicode {
                    Err("lone quantifier brackets".into())
                } else if self.braced_quantifier().is_some() {
                    Err("nothing to repeat".into())
                } else {
                    self.pos += 1;
                    Ok(self.char_node(c))
                }
            }
            Some('}' | ']') if self.unicode => Err("lone quantifier brackets".into()),
            Some(')' | '|') => Err("unexpected character".into()),
            _ => {
                self.pos += 1;
                Ok(self.char_node(c))
            }
        }
    }

    fn parse_group(&mut self) -> R<Node> {
        self.pos += 1;
        if !self.eat('?') {
            self.groups += 1;
            let index = self.groups;
            let node = self.parse_disjunction()?;
            self.expect(')', "unterminated group")?;
            return Ok(Node::Group {
                index,
                node: Box::new(node),
            });
        }
        match self.peek().and_then(char::from_u32) {
            Some(':') => {
                self.pos += 1;
                let node = self.parse_disjunction()?;
                self.expect(')', "unterminated group")?;
                Ok(node)
            }
            Some('<') => {
                self.pos += 1;
                let name = self.parse_group_name()?;
                self.groups += 1;
                let index = self.groups;
                let path = self.path.clone();
                for g in &self.names {
                    if g.name == name && might_both_participate(&g.path, &path) {
                        return Err("duplicate capture group name".into());
                    }
                }
                self.names.push(NamedGroup { name, index, path });
                let node = self.parse_disjunction()?;
                self.expect(')', "unterminated group")?;
                Ok(Node::Group {
                    index,
                    node: Box::new(node),
                })
            }
            Some('i' | 'm' | 's' | '-') => self.parse_modifiers(),
            _ => Err("invalid group".into()),
        }
    }

    /// `(?ims-ims:Disjunction)` after `(?`.
    fn parse_modifiers(&mut self) -> R<Node> {
        let invalid = || "invalid regular expression modifiers".to_string();
        let flag_bit = |c: Option<u32>| match c.and_then(char::from_u32) {
            Some('i') => 1u8,
            Some('m') => 2,
            Some('s') => 4,
            _ => 0,
        };
        let (mut add, mut remove) = (0u8, 0u8);
        loop {
            let bit = flag_bit(self.peek());
            if bit == 0 {
                break;
            }
            if add & bit != 0 {
                return Err(invalid());
            }
            add |= bit;
            self.pos += 1;
        }
        if self.eat('-') {
            loop {
                let bit = flag_bit(self.peek());
                if bit == 0 {
                    break;
                }
                if (add | remove) & bit != 0 {
                    return Err(invalid());
                }
                remove |= bit;
                self.pos += 1;
            }
            if add == 0 && remove == 0 {
                return Err(invalid());
            }
        }
        if !self.eat(':') {
            return Err("invalid group".into());
        }
        let saved = (self.ci, self.multiline, self.dot_all);
        let apply = |bit: u8, current: bool| {
            if add & bit != 0 {
                true
            } else if remove & bit != 0 {
                false
            } else {
                current
            }
        };
        self.ci = apply(1, self.ci);
        self.multiline = apply(2, self.multiline);
        self.dot_all = apply(4, self.dot_all);
        let node = self.parse_disjunction();
        (self.ci, self.multiline, self.dot_all) = saved;
        let node = node?;
        self.expect(')', "unterminated group")?;
        Ok(node)
    }

    /// RegExpIdentifierName followed by `>` (after `<`).
    fn parse_group_name(&mut self) -> R<Vec<u32>> {
        let invalid = || "invalid capture group name".to_string();
        let mut name = Vec::new();
        loop {
            if self.at('>') {
                if name.is_empty() {
                    return Err(invalid());
                }
                self.pos += 1;
                return Ok(name);
            }
            let Some(c) = self.peek() else {
                return Err(invalid());
            };
            self.pos += 1;
            let cp = if c == ch('\\') {
                if !self.eat('u') {
                    return Err(invalid());
                }
                self.unicode_escape(true).ok_or_else(invalid)?
            } else if !self.unicode && is_lead(c) && matches!(self.peek(), Some(t) if is_trail(t)) {
                let t = self.peek().expect("trail surrogate");
                self.pos += 1;
                0x10000 + ((c - 0xD800) << 10) + (t - 0xDC00)
            } else {
                c
            };
            let ok = match char::from_u32(cp) {
                Some(c) if name.is_empty() => unicode::is_id_start(c),
                Some(c) => unicode::is_id_continue(c),
                None => false,
            };
            if !ok {
                return Err(invalid());
            }
            name.push(cp);
        }
    }

    /// RegExpUnicodeEscapeSequence after `\u`; `None` (with the position
    /// restored) if the text does not form one.
    fn unicode_escape(&mut self, unicode_mode: bool) -> Option<u32> {
        let start = self.pos;
        if unicode_mode && self.eat('{') {
            let mut value = 0u32;
            let mut n = 0;
            while let Some(d) = self.peek().and_then(hex_value) {
                value = value.saturating_mul(16).saturating_add(d);
                n += 1;
                self.pos += 1;
            }
            if n > 0 && value <= MAX_CODE_POINT && self.eat('}') {
                return Some(value);
            }
            self.pos = start;
            return None;
        }
        let Some(lead) = self.hex_digits(4) else {
            self.pos = start;
            return None;
        };
        if unicode_mode && is_lead(lead) {
            let save = self.pos;
            if self.eat('\\') && self.eat('u') {
                if let Some(trail) = self.hex_digits(4)
                    && is_trail(trail)
                {
                    return Some(0x10000 + ((lead - 0xD800) << 10) + (trail - 0xDC00));
                }
            }
            self.pos = save;
        }
        Some(lead)
    }

    /// Exactly `n` hex digits; `None` (nothing consumed) otherwise.
    fn hex_digits(&mut self, n: usize) -> Option<u32> {
        let mut value = 0;
        for k in 0..n {
            value = value * 16 + self.peek_at(k).and_then(hex_value)?;
        }
        self.pos += n;
        Some(value)
    }

    /// LegacyOctalEscapeSequence (Annex B), at its first digit.
    fn legacy_octal(&mut self) -> u32 {
        let first = self.peek().expect("octal digit") - ch('0');
        self.pos += 1;
        let mut value = first;
        if is_octal(self.peek()) {
            value = value * 8 + (self.peek().expect("octal digit") - ch('0'));
            self.pos += 1;
            if first <= 3 && is_octal(self.peek()) {
                value = value * 8 + (self.peek().expect("octal digit") - ch('0'));
                self.pos += 1;
            }
        }
        value
    }

    /// `\` AtomEscape (at the backslash).
    fn parse_atom_escape(&mut self) -> R<Node> {
        self.pos += 1;
        let Some(c) = self.peek() else {
            return Err("\\ at end of pattern".into());
        };
        if (ch('1')..=ch('9')).contains(&c) {
            let start = self.pos;
            let mut n = 0u32;
            while is_digit(self.peek()) {
                n = n
                    .saturating_mul(10)
                    .saturating_add(self.peek().expect("digit") - ch('0'));
                self.pos += 1;
            }
            if n <= self.total_groups {
                return Ok(Node::BackRef {
                    groups: vec![n],
                    ci: self.ci,
                });
            }
            if self.unicode {
                return Err("invalid escape".into());
            }
            // Annex B: not a backreference, so a legacy octal escape or an
            // identity escape of 8 or 9.
            self.pos = start;
        }
        match char::from_u32(c) {
            Some('k') if self.named => {
                self.pos += 1;
                if !self.eat('<') {
                    return Err("invalid named reference".into());
                }
                let name = self.parse_group_name()?;
                self.refs.push(name.clone());
                Ok(Node::NamedBackRef { name, ci: self.ci })
            }
            Some('d' | 'D' | 's' | 'S' | 'w' | 'W') => {
                self.pos += 1;
                let set = self.class_escape_set(c);
                Ok(self.set_node(set, false, self.sets))
            }
            Some('p' | 'P') if self.unicode => {
                self.pos += 1;
                let value = self.property_escape(c == ch('P'))?;
                if self.sets {
                    Ok(self.class_value_node(value))
                } else {
                    Ok(self.set_node(value.set, false, false))
                }
            }
            _ => match self.character_escape(false)? {
                Some(v) => Ok(self.char_node(v)),
                // Annex B: `\` before a `c` that starts no control escape
                // matches a backslash; the `c` is parsed next.
                None => Ok(self.char_node(ch('\\'))),
            },
        }
    }

    /// CharacterEscape (and IdentityEscape), at the character after the
    /// backslash. `None` is the Annex B backslash before a lone `c`.
    fn character_escape(&mut self, in_class: bool) -> R<Option<u32>> {
        let Some(c) = self.peek() else {
            return Err("\\ at end of pattern".into());
        };
        let simple = match char::from_u32(c) {
            Some('f') => Some(0x0C),
            Some('n') => Some(0x0A),
            Some('r') => Some(0x0D),
            Some('t') => Some(0x09),
            Some('v') => Some(0x0B),
            _ => None,
        };
        if let Some(v) = simple {
            self.pos += 1;
            return Ok(Some(v));
        }
        match char::from_u32(c) {
            Some('c') => {
                let next = self.peek_at(1);
                match next {
                    Some(l) if is_ascii_letter(l) => {
                        self.pos += 2;
                        Ok(Some(l % 32))
                    }
                    Some(l) if !self.unicode && in_class && (is_digit(Some(l)) || l == ch('_')) => {
                        self.pos += 2;
                        Ok(Some(l % 32))
                    }
                    _ if self.unicode => Err("invalid unicode escape".into()),
                    _ => Ok(None),
                }
            }
            Some('0'..='9') => {
                if c == ch('0') && !is_digit(self.peek_at(1)) {
                    self.pos += 1;
                    return Ok(Some(0));
                }
                if self.unicode {
                    return Err(if in_class {
                        "invalid class escape".into()
                    } else {
                        "invalid decimal escape".into()
                    });
                }
                if c >= ch('8') {
                    self.pos += 1;
                    return Ok(Some(c));
                }
                Ok(Some(self.legacy_octal()))
            }
            Some('x') => {
                self.pos += 1;
                match self.hex_digits(2) {
                    Some(v) => Ok(Some(v)),
                    None if self.unicode => Err("invalid escape".into()),
                    None => Ok(Some(ch('x'))),
                }
            }
            Some('u') => {
                self.pos += 1;
                match self.unicode_escape(self.unicode) {
                    Some(v) => Ok(Some(v)),
                    None if self.unicode => Err("invalid unicode escape".into()),
                    None => Ok(Some(ch('u'))),
                }
            }
            _ => {
                if self.unicode {
                    if is_syntax_char(c) || c == ch('/') || (in_class && c == ch('-')) {
                        self.pos += 1;
                        return Ok(Some(c));
                    }
                    return Err("invalid escape".into());
                }
                if c == ch('k') && self.named {
                    return Err("invalid escape".into());
                }
                self.pos += 1;
                Ok(Some(c))
            }
        }
    }

    // ----- legacy and `u` classes -----

    /// CharacterClass after `[`.
    fn parse_class(&mut self) -> R<Node> {
        if self.sets {
            let value = self.parse_class_v()?;
            return Ok(self.class_value_node(value));
        }
        let invert = self.eat('^');
        let mut set = CharSet::new();
        loop {
            let Some(c) = self.peek() else {
                return Err("unterminated character class".into());
            };
            if c == ch(']') {
                self.pos += 1;
                break;
            }
            let a = self.class_atom()?;
            let range = self.at('-') && matches!(self.peek_at(1), Some(n) if n != ch(']'));
            if !range {
                add_atom(&mut set, a);
                continue;
            }
            self.pos += 1;
            let b = self.class_atom()?;
            match (a, b) {
                (ClassAtom::Char(x), ClassAtom::Char(y)) => {
                    if x > y {
                        return Err("range out of order in character class".into());
                    }
                    set.add_range(x, y);
                }
                (a, b) => {
                    if self.unicode {
                        return Err("invalid character class".into());
                    }
                    add_atom(&mut set, a);
                    set.add(ch('-'));
                    add_atom(&mut set, b);
                }
            }
        }
        Ok(self.set_node(set, invert, false))
    }

    fn class_atom(&mut self) -> R<ClassAtom> {
        let c = self.peek().expect("a class atom");
        self.pos += 1;
        if c != ch('\\') {
            return Ok(ClassAtom::Char(c));
        }
        let Some(e) = self.peek() else {
            return Err("\\ at end of pattern".into());
        };
        match char::from_u32(e) {
            Some('b') => {
                self.pos += 1;
                Ok(ClassAtom::Char(0x08))
            }
            Some('-') if self.unicode => {
                self.pos += 1;
                Ok(ClassAtom::Char(e))
            }
            Some('d' | 'D' | 's' | 'S' | 'w' | 'W') => {
                self.pos += 1;
                Ok(ClassAtom::Set(self.class_escape_set(e)))
            }
            Some('p' | 'P') if self.unicode => {
                self.pos += 1;
                Ok(ClassAtom::Set(self.property_escape(e == ch('P'))?.set))
            }
            _ => match self.character_escape(true)? {
                Some(v) => Ok(ClassAtom::Char(v)),
                None => Ok(ClassAtom::Char(ch('\\'))),
            },
        }
    }

    // ----- `v` classes -----

    /// ClassContents of a `v` class after `[`, through the closing `]`.
    fn parse_class_v(&mut self) -> R<ClassValue> {
        self.enter()?;
        let invert = self.eat('^');
        let value = self.parse_class_set_expression()?;
        self.leave();
        if !invert {
            return Ok(value);
        }
        if value.may_strings {
            return Err("negated character class may contain strings".into());
        }
        Ok(ClassValue::chars(self.complement(&value.set)))
    }

    /// ClassSetExpression through the closing `]`.
    fn parse_class_set_expression(&mut self) -> R<ClassValue> {
        if self.eat(']') {
            return Ok(ClassValue::default());
        }
        let (first, first_is_range) = self.parse_class_set_range_or_operand()?;
        if self.at2('&', '&') || self.at2('-', '-') {
            if first_is_range {
                return Err("invalid set operation in character class".into());
            }
            let intersection = self.at('&');
            let mut acc = first;
            loop {
                let more = if intersection {
                    self.at2('&', '&')
                } else {
                    self.at2('-', '-')
                };
                if !more {
                    break;
                }
                self.pos += 2;
                if intersection && self.at('&') {
                    return Err("invalid set operation in character class".into());
                }
                let operand = match self.parse_class_set_operand()? {
                    Operand::Char(c) => ClassValue::chars(CharSet::single(self.maybe_fold_char(c))),
                    Operand::Class(v) => v,
                };
                let may = if intersection {
                    acc.may_strings && operand.may_strings
                } else {
                    acc.may_strings
                };
                acc = if intersection {
                    acc.intersect(&operand)
                } else {
                    acc.subtract(&operand)
                };
                acc.may_strings = may;
            }
            if !self.eat(']') {
                return Err("invalid set operation in character class".into());
            }
            return Ok(acc);
        }
        let mut acc = first;
        loop {
            if self.eat(']') {
                return Ok(acc);
            }
            if self.peek().is_none() {
                return Err("unterminated character class".into());
            }
            if self.at2('&', '&') || self.at2('-', '-') {
                return Err("invalid set operation in character class".into());
            }
            let (next, _) = self.parse_class_set_range_or_operand()?;
            acc = acc.union(next);
        }
    }

    /// A ClassSetRange or a ClassSetOperand of a ClassUnion.
    fn parse_class_set_range_or_operand(&mut self) -> R<(ClassValue, bool)> {
        match self.parse_class_set_operand()? {
            Operand::Class(v) => Ok((v, false)),
            Operand::Char(a) => {
                if self.at('-') && !self.at2('-', '-') {
                    self.pos += 1;
                    let Operand::Char(b) = self.parse_class_set_operand()? else {
                        return Err("invalid character class range".into());
                    };
                    if a > b {
                        return Err("range out of order in character class".into());
                    }
                    let set = self.maybe_fold(CharSet::range(a, b));
                    return Ok((ClassValue::chars(set), true));
                }
                Ok((
                    ClassValue::chars(CharSet::single(self.maybe_fold_char(a))),
                    false,
                ))
            }
        }
    }

    fn parse_class_set_operand(&mut self) -> R<Operand> {
        let Some(c) = self.peek() else {
            return Err("unterminated character class".into());
        };
        if c == ch('[') {
            self.pos += 1;
            return Ok(Operand::Class(self.parse_class_v()?));
        }
        if c == ch('\\') {
            let e = self.peek_at(1);
            match e.and_then(char::from_u32) {
                Some(l @ ('d' | 'D' | 's' | 'S' | 'w' | 'W')) => {
                    self.pos += 2;
                    return Ok(Operand::Class(ClassValue::chars(
                        self.class_escape_set(ch(l)),
                    )));
                }
                Some(l @ ('p' | 'P')) => {
                    self.pos += 2;
                    return Ok(Operand::Class(self.property_escape(l == 'P')?));
                }
                Some('q') => {
                    self.pos += 2;
                    if !self.eat('{') {
                        return Err("invalid escape".into());
                    }
                    return Ok(Operand::Class(self.class_string_disjunction()?));
                }
                _ => {}
            }
        }
        Ok(Operand::Char(self.class_set_character()?))
    }

    /// ClassStringDisjunctionContents after `\q{`, through the `}`.
    fn class_string_disjunction(&mut self) -> R<ClassValue> {
        let mut value = ClassValue::default();
        let mut current = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err("unterminated class string disjunction".into());
            };
            if c == ch('}') || c == ch('|') {
                self.pos += 1;
                let s = std::mem::take(&mut current);
                if s.len() == 1 {
                    value.set.add(self.maybe_fold_char(s[0]));
                } else {
                    value.may_strings = true;
                    value.strings.push(self.maybe_fold_string(s));
                }
                if c == ch('}') {
                    break;
                }
                continue;
            }
            current.push(self.class_set_character()?);
        }
        value.strings.sort();
        value.strings.dedup();
        Ok(value)
    }

    /// ClassSetCharacter.
    fn class_set_character(&mut self) -> R<u32> {
        let Some(c) = self.peek() else {
            return Err("unterminated character class".into());
        };
        if c == ch('\\') {
            self.pos += 1;
            let Some(e) = self.peek() else {
                return Err("\\ at end of pattern".into());
            };
            if e == ch('b') {
                self.pos += 1;
                return Ok(0x08);
            }
            if matches!(
                char::from_u32(e),
                Some(
                    '&' | '-'
                        | '!'
                        | '#'
                        | '%'
                        | ','
                        | ':'
                        | ';'
                        | '<'
                        | '='
                        | '>'
                        | '@'
                        | '`'
                        | '~'
                )
            ) {
                self.pos += 1;
                return Ok(e);
            }
            return match self.character_escape(true)? {
                Some(v) => Ok(v),
                None => Err("invalid escape".into()),
            };
        }
        if matches!(
            char::from_u32(c),
            Some('(' | ')' | '[' | ']' | '{' | '}' | '/' | '-' | '|')
        ) {
            return Err("invalid character in character class".into());
        }
        let double = matches!(
            char::from_u32(c),
            Some(
                '&' | '!'
                    | '#'
                    | '$'
                    | '%'
                    | '*'
                    | '+'
                    | ','
                    | '.'
                    | ':'
                    | ';'
                    | '<'
                    | '='
                    | '>'
                    | '?'
                    | '@'
                    | '^'
                    | '`'
                    | '~'
            )
        );
        if double && self.peek_at(1) == Some(c) {
            return Err("invalid set operation in character class".into());
        }
        self.pos += 1;
        Ok(c)
    }
}

fn add_atom(set: &mut CharSet, atom: ClassAtom) {
    match atom {
        ClassAtom::Char(c) => set.add(c),
        ClassAtom::Set(s) => set.add_set(&s),
    }
}

/// MightBothParticipate(x, y): false only when the two groups are in
/// different alternatives of the same Disjunction.
fn might_both_participate(a: &[(u32, u32)], b: &[(u32, u32)]) -> bool {
    for (x, y) in a.iter().zip(b) {
        if x != y {
            return x.0 != y.0;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(s: &str) -> Flags {
        Flags::parse(&crate::string::utf16(s)).expect("valid flags")
    }

    fn chars(s: &str, unicode: bool) -> Vec<u32> {
        let units = crate::string::utf16(s);
        if unicode {
            unicode::code_points(&units)
        } else {
            units.iter().map(|&u| u32::from(u)).collect()
        }
    }

    fn ok(p: &str, f: &str) -> bool {
        let fl = flags(f);
        parse(&chars(p, fl.unicode || fl.unicode_sets), fl).is_ok()
    }

    #[test]
    fn annex_b_and_unicode_mode() {
        assert!(ok("]", "") && !ok("]", "u"));
        assert!(ok("a{", "") && !ok("a{", "u"));
        assert!(!ok("{1}", ""));
        assert!(ok("\\c", "") && !ok("\\c", "u"));
        assert!(ok("\\8", "") && !ok("\\8", "u"));
        assert!(ok("(?=a)*", "") && !ok("(?=a)*", "u"));
        assert!(!ok("(?<=a)*", ""));
        assert!(ok("[\\d-a]", "") && !ok("[\\d-a]", "u"));
        assert!(!ok("[z-a]", ""));
        assert!(!ok("a{2,1}", ""));
        assert!(ok("\\k", "") && !ok("\\k(?<a>.)", ""));
        assert!(ok("(?<a>.)\\k<a>", "") && !ok("(?<a>.)\\k<b>", ""));
    }

    #[test]
    fn groups_and_modifiers() {
        assert!(ok("(?<a>x)|(?<a>y)", ""));
        assert!(!ok("(?<a>x)(?<a>y)", ""));
        assert!(!ok("(?:(?<a>x)|(?<a>y))(?<a>z)", ""));
        assert!(ok("(?i:a)(?-i:b)(?m-s:c)", ""));
        assert!(!ok("(?ii:a)", "") && !ok("(?i-i:a)", "") && !ok("(?-:a)", ""));
        assert!(!ok("(?x:a)", ""));
    }

    #[test]
    fn unicode_sets() {
        assert!(ok("[[a-z]&&[aeiou]]", "v") && !ok("[a-z&&[aeiou]]", "v"));
        assert!(ok("[\\w--\\d]", "v"));
        assert!(!ok("[a-z&&b--c]", "v"));
        assert!(!ok("[a&&&b]", "v"));
        assert!(!ok("[(]", "v"));
        assert!(ok("[\\q{abc|d}]", "v"));
        assert!(!ok("[^\\q{abc}]", "v"));
        assert!(ok("[^\\q{a}]", "v"));
        assert!(ok("\\p{RGI_Emoji}", "v") && !ok("\\p{RGI_Emoji}", "u"));
        assert!(!ok("\\P{RGI_Emoji}", "v"));
    }
}
