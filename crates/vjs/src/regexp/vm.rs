//! The backtracking matcher. It never recurses: choice points and the
//! undo records of captures and registers live on one explicit stack, so a
//! pattern that backtracks a lot (or a loop with many iterations) uses
//! heap memory, not native stack.
//!
//! Positions are code unit indices. With the `u` or `v` flag a character
//! is a code point: a surrogate pair is read as one character and a lone
//! surrogate as itself (ECMA-262 §22.2.2: the input is the list of code
//! points of the string).

use super::charset::{canonicalize, is_line_terminator, is_word_char};
use super::compiler::{Atom, Inst, Program};

/// An unset capture slot.
pub const UNSET: usize = usize::MAX;

/// The backtracking stack may not grow past this many entries; a match
/// that needs more fails with [`StackExhausted`].
pub const MAX_STACK: usize = 1 << 24;

/// The match needed more backtracking state than [`MAX_STACK`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackExhausted;

#[derive(Clone, Copy, Debug)]
enum Entry {
    /// Resume at `pc` with `pos`.
    Choice { pc: u32, pos: usize },
    /// Restore a capture slot.
    Cap { slot: u32, old: usize },
    /// Restore a register.
    Reg { reg: u32, old: usize },
    /// The bottom of a running lookaround. Reaching it while backtracking
    /// means the body failed: a negative lookaround then succeeds at `cont`.
    Look { pos: usize, cont: u32, negate: bool },
    /// A greedy [`Inst::Simple`] at `at` that can give back characters
    /// down to `min_pos`.
    Greedy { at: u32, min_pos: usize, pos: usize },
    /// A lazy [`Inst::Simple`] at `at` that can take one more character.
    Lazy { at: u32, pos: usize, count: u64 },
}

impl Entry {
    fn is_undo(&self) -> bool {
        matches!(self, Entry::Cap { .. } | Entry::Reg { .. })
    }
}

pub struct Vm<'a> {
    prog: &'a Program,
    input: &'a [u16],
    /// Capture slots: `2 * i` and `2 * i + 1` are the start and end of
    /// group `i` (0 is the whole match).
    pub caps: Vec<usize>,
    regs: Vec<usize>,
    stack: Vec<Entry>,
}

fn is_lead(u: u16) -> bool {
    (0xD800..=0xDBFF).contains(&u)
}

fn is_trail(u: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&u)
}

fn combine(lead: u16, trail: u16) -> u32 {
    0x10000 + ((u32::from(lead) - 0xD800) << 10) + (u32::from(trail) - 0xDC00)
}

impl<'a> Vm<'a> {
    pub fn new(prog: &'a Program, input: &'a [u16]) -> Vm<'a> {
        Vm {
            prog,
            input,
            caps: vec![UNSET; prog.slots],
            regs: vec![0; prog.regs],
            stack: Vec::new(),
        }
    }

    /// The character at `pos` and the position after it, reading no
    /// further than `limit`.
    fn char_at(&self, pos: usize, limit: usize) -> Option<(u32, usize)> {
        if pos >= limit {
            return None;
        }
        let u = self.input[pos];
        if self.prog.unicode && is_lead(u) && pos + 1 < limit && is_trail(self.input[pos + 1]) {
            return Some((combine(u, self.input[pos + 1]), pos + 2));
        }
        Some((u32::from(u), pos + 1))
    }

    /// The character before `pos` and its position, reading no further
    /// back than `low`.
    fn char_before(&self, pos: usize, low: usize) -> Option<(u32, usize)> {
        if pos <= low {
            return None;
        }
        let u = self.input[pos - 1];
        if self.prog.unicode && is_trail(u) && pos >= low + 2 && is_lead(self.input[pos - 2]) {
            return Some((combine(self.input[pos - 2], u), pos - 2));
        }
        Some((u32::from(u), pos - 1))
    }

    fn canon(&self, c: u32) -> u32 {
        canonicalize(c, self.prog.fold)
    }

    fn atom_matches(&self, atom: &Atom, c: u32) -> bool {
        match *atom {
            Atom::Char { c: k, ci } => {
                if ci {
                    self.canon(c) == k
                } else {
                    c == k
                }
            }
            Atom::Set { set, invert, ci } => {
                let x = if ci { self.canon(c) } else { c };
                self.prog.sets[set as usize].contains(x) != invert
            }
            Atom::Any { dot_all } => dot_all || !is_line_terminator(c),
        }
    }

    /// Matches one character in the given direction.
    fn step(&self, atom: &Atom, pos: usize, back: bool) -> Option<usize> {
        let (c, next) = if back {
            self.char_before(pos, 0)?
        } else {
            self.char_at(pos, self.input.len())?
        };
        self.atom_matches(atom, c).then_some(next)
    }

    fn push(&mut self, e: Entry) {
        self.stack.push(e);
    }

    fn set_cap(&mut self, slot: u32, value: usize) {
        let old = self.caps[slot as usize];
        self.push(Entry::Cap { slot, old });
        self.caps[slot as usize] = value;
    }

    fn set_reg(&mut self, reg: u32, value: usize) {
        let old = self.regs[reg as usize];
        self.push(Entry::Reg { reg, old });
        self.regs[reg as usize] = value;
    }

    fn undo(&mut self, e: Entry) {
        match e {
            Entry::Cap { slot, old } => self.caps[slot as usize] = old,
            Entry::Reg { reg, old } => self.regs[reg as usize] = old,
            _ => {}
        }
    }

    fn same(&self, a: u32, b: u32, ci: bool) -> bool {
        if ci {
            self.canon(a) == self.canon(b)
        } else {
            a == b
        }
    }

    /// BackreferenceMatcher: the new position, or `None` on failure.
    fn backref(&self, groups: &[u32], ci: bool, back: bool, pos: usize) -> Option<usize> {
        let defined = groups.iter().find(|&&g| {
            self.caps[2 * g as usize] != UNSET && self.caps[2 * g as usize + 1] != UNSET
        });
        let Some(&g) = defined else {
            return Some(pos);
        };
        let (s, e) = (self.caps[2 * g as usize], self.caps[2 * g as usize + 1]);
        let input = self.input;
        if !self.prog.unicode && !ci {
            let len = e - s;
            if !back {
                if pos + len > input.len() || input[s..e] != input[pos..pos + len] {
                    return None;
                }
                return Some(pos + len);
            }
            if pos < len || input[s..e] != input[pos - len..pos] {
                return None;
            }
            return Some(pos - len);
        }
        if !back {
            let (mut i, mut j) = (s, pos);
            while let Some((c1, i2)) = self.char_at(i, e) {
                let (c2, j2) = self.char_at(j, input.len())?;
                if !self.same(c1, c2, ci) {
                    return None;
                }
                (i, j) = (i2, j2);
            }
            Some(j)
        } else {
            let (mut i, mut j) = (e, pos);
            while let Some((c1, i2)) = self.char_before(i, s) {
                let (c2, j2) = self.char_before(j, 0)?;
                if !self.same(c1, c2, ci) {
                    return None;
                }
                (i, j) = (i2, j2);
            }
            Some(j)
        }
    }

    /// Runs the matcher from `start`. On success the capture slots hold
    /// the match.
    pub fn run(&mut self, start: usize) -> Result<bool, StackExhausted> {
        self.caps.iter_mut().for_each(|c| *c = UNSET);
        self.regs.iter_mut().for_each(|r| *r = 0);
        self.stack.clear();
        self.caps[0] = start;
        let prog = self.prog;
        let input = self.input;
        let len = input.len();
        let mut pc = 0usize;
        let mut pos = start;
        loop {
            let ok = match prog.insts[pc] {
                Inst::Atom { atom, back } => match self.step(&atom, pos, back) {
                    Some(p) => {
                        pos = p;
                        pc += 1;
                        true
                    }
                    None => false,
                },
                Inst::LineStart { multiline } => {
                    let ok =
                        pos == 0 || (multiline && is_line_terminator(u32::from(input[pos - 1])));
                    pc += 1;
                    ok
                }
                Inst::LineEnd { multiline } => {
                    let ok = pos == len || (multiline && is_line_terminator(u32::from(input[pos])));
                    pc += 1;
                    ok
                }
                Inst::WordBoundary { negate, unicode_ci } => {
                    let a = pos > 0 && is_word_char(u32::from(input[pos - 1]), unicode_ci);
                    let b = pos < len && is_word_char(u32::from(input[pos]), unicode_ci);
                    pc += 1;
                    (a != b) != negate
                }
                Inst::BackRef { refs, ci, back } => {
                    match self.backref(&prog.backrefs[refs as usize], ci, back, pos) {
                        Some(p) => {
                            pos = p;
                            pc += 1;
                            true
                        }
                        None => false,
                    }
                }
                Inst::Save(slot) => {
                    self.set_cap(slot, pos);
                    pc += 1;
                    true
                }
                Inst::ClearCaps { from, to } => {
                    for slot in from..to {
                        if self.caps[slot as usize] != UNSET {
                            self.set_cap(slot, UNSET);
                        }
                    }
                    pc += 1;
                    true
                }
                Inst::Split { prefer, other } => {
                    self.push(Entry::Choice { pc: other, pos });
                    pc = prefer as usize;
                    true
                }
                Inst::Jmp(target) => {
                    pc = target as usize;
                    true
                }
                Inst::RepeatInit { reg } => {
                    self.set_reg(reg, 0);
                    pc += 1;
                    true
                }
                Inst::RepeatBranch {
                    reg,
                    min,
                    max,
                    greedy,
                    exit,
                } => {
                    let count = self.regs[reg as usize] as u64;
                    if count >= max {
                        pc = exit as usize;
                    } else if count < min {
                        pc += 1;
                    } else if greedy {
                        self.push(Entry::Choice { pc: exit, pos });
                        pc += 1;
                    } else {
                        self.push(Entry::Choice {
                            pc: pc as u32 + 1,
                            pos,
                        });
                        pc = exit as usize;
                    }
                    true
                }
                Inst::RepeatStart { pos_reg } => {
                    self.set_reg(pos_reg, pos);
                    pc += 1;
                    true
                }
                Inst::RepeatEnd {
                    reg,
                    pos_reg,
                    min,
                    head,
                } => {
                    let count = self.regs[reg as usize] as u64;
                    if count >= min && pos == self.regs[pos_reg as usize] {
                        false
                    } else {
                        self.set_reg(reg, (count + 1) as usize);
                        pc = head as usize;
                        true
                    }
                }
                Inst::Simple {
                    atom,
                    min,
                    max,
                    greedy,
                } => {
                    let mut p = pos;
                    let mut n = 0u64;
                    let mut ok = true;
                    while n < min {
                        match self.step(&atom, p, false) {
                            Some(q) => {
                                p = q;
                                n += 1;
                            }
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        let at = pc as u32;
                        if greedy {
                            let min_pos = p;
                            while n < max {
                                match self.step(&atom, p, false) {
                                    Some(q) => {
                                        p = q;
                                        n += 1;
                                    }
                                    None => break,
                                }
                            }
                            if p != min_pos {
                                self.push(Entry::Greedy {
                                    at,
                                    min_pos,
                                    pos: p,
                                });
                            }
                        } else if n < max {
                            self.push(Entry::Lazy {
                                at,
                                pos: p,
                                count: n,
                            });
                        }
                        pos = p;
                        pc += 1;
                    }
                    ok
                }
                Inst::LookStart { reg, negate, cont } => {
                    let old = self.regs[reg as usize];
                    self.push(Entry::Reg { reg, old });
                    self.regs[reg as usize] = self.stack.len();
                    self.push(Entry::Look { pos, cont, negate });
                    pc += 1;
                    true
                }
                Inst::LookEnd { reg, negate } => {
                    let depth = self.regs[reg as usize];
                    let saved = match self.stack[depth] {
                        Entry::Look { pos, .. } => pos,
                        _ => unreachable!("lookaround without its stack entry"),
                    };
                    if negate {
                        // The body matched: undo it and fail.
                        while self.stack.len() > depth + 1 {
                            let e = self.stack.pop().expect("entry above the lookaround");
                            self.undo(e);
                        }
                        self.stack.pop();
                        false
                    } else {
                        // The body matched: drop its choice points but keep
                        // the undo records of its captures.
                        let mut w = depth;
                        for r in depth + 1..self.stack.len() {
                            let e = self.stack[r];
                            if e.is_undo() {
                                self.stack[w] = e;
                                w += 1;
                            }
                        }
                        self.stack.truncate(w);
                        pos = saved;
                        pc += 1;
                        true
                    }
                }
                Inst::Match => {
                    self.caps[1] = pos;
                    return Ok(true);
                }
            };
            if self.stack.len() > MAX_STACK {
                return Err(StackExhausted);
            }
            if ok {
                continue;
            }
            // Backtrack.
            loop {
                let Some(e) = self.stack.pop() else {
                    return Ok(false);
                };
                match e {
                    Entry::Choice { pc: p, pos: q } => {
                        pc = p as usize;
                        pos = q;
                        break;
                    }
                    Entry::Cap { .. } | Entry::Reg { .. } => self.undo(e),
                    Entry::Look {
                        pos: q,
                        cont,
                        negate,
                    } => {
                        if negate {
                            pc = cont as usize;
                            pos = q;
                            break;
                        }
                    }
                    Entry::Greedy {
                        at,
                        min_pos,
                        pos: p,
                    } => {
                        let q = match self.char_before(p, min_pos) {
                            Some((_, q)) => q,
                            None => min_pos,
                        };
                        if q > min_pos {
                            self.push(Entry::Greedy {
                                at,
                                min_pos,
                                pos: q,
                            });
                        }
                        pc = at as usize + 1;
                        pos = q;
                        break;
                    }
                    Entry::Lazy { at, pos: p, count } => {
                        let Inst::Simple { atom, max, .. } = prog.insts[at as usize] else {
                            unreachable!("lazy entry without its instruction");
                        };
                        if let Some(q) = self.step(&atom, p, false) {
                            if count + 1 < max {
                                self.push(Entry::Lazy {
                                    at,
                                    pos: q,
                                    count: count + 1,
                                });
                            }
                            pc = at as usize + 1;
                            pos = q;
                            break;
                        }
                    }
                }
            }
        }
    }
}
