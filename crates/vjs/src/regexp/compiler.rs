//! Compiles a pattern tree into the backtracking bytecode of
//! [`super::vm`].
//!
//! Lookbehind bodies are compiled in the backward direction (ECMA-262
//! §22.2.2: sequences run right to left, characters are read before the
//! current position, and a group records its end before its start).

use super::charset::{CaseFold, CharSet};
use super::parser::{Node, Pattern};

/// A matcher of one character.
#[derive(Clone, Copy, Debug)]
pub enum Atom {
    /// With `ci`, `c` is canonical and the input character is
    /// canonicalized before comparing.
    Char {
        c: u32,
        ci: bool,
    },
    Set {
        set: u32,
        invert: bool,
        ci: bool,
    },
    Any {
        dot_all: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum Inst {
    /// Matches one character forward (or backward with `back`).
    Atom {
        atom: Atom,
        back: bool,
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
    BackRef {
        refs: u32,
        ci: bool,
        back: bool,
    },
    /// Records the position in a capture slot.
    Save(u32),
    /// Resets the capture slots `from..to`.
    ClearCaps {
        from: u32,
        to: u32,
    },
    /// Continues at `prefer`; backtracking resumes at `other`.
    Split {
        prefer: u32,
        other: u32,
    },
    Jmp(u32),
    /// Sets an iteration counter to 0.
    RepeatInit {
        reg: u32,
    },
    /// Decides between one more iteration (the next instruction) and the
    /// exit, by the counter and the bounds.
    RepeatBranch {
        reg: u32,
        min: u64,
        max: u64,
        greedy: bool,
        exit: u32,
    },
    /// Records where an iteration starts.
    RepeatStart {
        pos_reg: u32,
    },
    /// Ends an iteration: an empty iteration past the minimum fails;
    /// otherwise the counter grows and control returns to `head`.
    RepeatEnd {
        reg: u32,
        pos_reg: u32,
        min: u64,
        head: u32,
    },
    /// A quantified one-character atom (forward only), matched without a
    /// loop: greedy runs take as many characters as they can and give
    /// them back one at a time.
    Simple {
        atom: Atom,
        min: u64,
        max: u64,
        greedy: bool,
    },
    /// Opens a lookaround; `cont` follows its `LookEnd`.
    LookStart {
        reg: u32,
        negate: bool,
        cont: u32,
    },
    LookEnd {
        reg: u32,
        negate: bool,
    },
    Match,
}

/// A compiled pattern.
pub struct Program {
    pub insts: Vec<Inst>,
    pub sets: Vec<CharSet>,
    pub backrefs: Vec<Vec<u32>>,
    /// `2 * (groups + 1)`.
    pub slots: usize,
    pub regs: usize,
    /// Characters are code points (`u` or `v`).
    pub unicode: bool,
    pub fold: CaseFold,
    /// A code unit every match must start with (a plain first character),
    /// used to skip start positions.
    pub first_unit: Option<u16>,
}

pub fn compile(pattern: &Pattern, unicode: bool) -> Program {
    let mut comp = Compiler {
        insts: Vec::new(),
        sets: Vec::new(),
        backrefs: Vec::new(),
        regs: 0,
        names: &pattern.group_names,
    };
    comp.node(&pattern.node, false);
    comp.emit(Inst::Match);
    let first_unit = match comp.insts.first() {
        Some(Inst::Atom {
            atom: Atom::Char { c, ci: false },
            back: false,
        }) if *c <= 0xFFFF && !(0xD800..=0xDFFF).contains(c) => Some(*c as u16),
        _ => None,
    };
    Program {
        insts: comp.insts,
        sets: comp.sets,
        backrefs: comp.backrefs,
        slots: 2 * (pattern.group_count as usize + 1),
        regs: comp.regs as usize,
        unicode,
        fold: CaseFold::for_unicode(unicode),
        first_unit,
    }
}

struct Compiler<'a> {
    insts: Vec<Inst>,
    sets: Vec<CharSet>,
    backrefs: Vec<Vec<u32>>,
    regs: u32,
    names: &'a [Option<Vec<u32>>],
}

impl Compiler<'_> {
    fn emit(&mut self, inst: Inst) -> usize {
        self.insts.push(inst);
        self.insts.len() - 1
    }

    fn here(&self) -> u32 {
        self.insts.len() as u32
    }

    fn reg(&mut self) -> u32 {
        self.regs += 1;
        self.regs - 1
    }

    fn set(&mut self, set: &CharSet) -> u32 {
        self.sets.push(set.clone());
        (self.sets.len() - 1) as u32
    }

    /// The one-character matcher of a node, if it is one.
    fn atom(&mut self, node: &Node) -> Option<Atom> {
        match node {
            Node::Char { c, ci } => Some(Atom::Char { c: *c, ci: *ci }),
            Node::Set { set, invert, ci } => Some(Atom::Set {
                set: self.set(set),
                invert: *invert,
                ci: *ci,
            }),
            Node::Dot { dot_all } => Some(Atom::Any { dot_all: *dot_all }),
            _ => None,
        }
    }

    fn node(&mut self, node: &Node, back: bool) {
        match node {
            Node::Empty => {}
            Node::Char { .. } | Node::Set { .. } | Node::Dot { .. } => {
                let atom = self.atom(node).expect("a one-character node");
                self.emit(Inst::Atom { atom, back });
            }
            Node::Strings {
                strings,
                set,
                empty,
                ci,
            } => {
                // Longest strings first, then the single characters, then
                // the empty string (CompileAtom for a `v` class).
                let mut alternatives: Vec<Node> = strings
                    .iter()
                    .map(|s| Node::Seq(s.iter().map(|&c| Node::Char { c, ci: *ci }).collect()))
                    .collect();
                alternatives.push(Node::Set {
                    set: set.clone(),
                    invert: false,
                    ci: *ci,
                });
                if *empty {
                    alternatives.push(Node::Empty);
                }
                self.alternation(&alternatives, back);
            }
            Node::LineStart { multiline } => {
                self.emit(Inst::LineStart {
                    multiline: *multiline,
                });
            }
            Node::LineEnd { multiline } => {
                self.emit(Inst::LineEnd {
                    multiline: *multiline,
                });
            }
            Node::WordBoundary { negate, unicode_ci } => {
                self.emit(Inst::WordBoundary {
                    negate: *negate,
                    unicode_ci: *unicode_ci,
                });
            }
            Node::BackRef { groups, ci } => self.backref(groups.clone(), *ci, back),
            Node::NamedBackRef { name, ci } => {
                let groups = self
                    .names
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.as_ref() == Some(name))
                    .map(|(i, _)| i as u32 + 1)
                    .collect();
                self.backref(groups, *ci, back);
            }
            Node::Group { index, node } => {
                let (start, end) = (2 * index, 2 * index + 1);
                let (first, last) = if back { (end, start) } else { (start, end) };
                self.emit(Inst::Save(first));
                self.node(node, back);
                self.emit(Inst::Save(last));
            }
            Node::Look {
                behind,
                negate,
                node,
            } => {
                let reg = self.reg();
                let start = self.emit(Inst::LookStart {
                    reg,
                    negate: *negate,
                    cont: 0,
                });
                self.node(node, *behind);
                self.emit(Inst::LookEnd {
                    reg,
                    negate: *negate,
                });
                let cont = self.here();
                if let Inst::LookStart { cont: c, .. } = &mut self.insts[start] {
                    *c = cont;
                }
            }
            Node::Repeat {
                node,
                min,
                max,
                greedy,
                first_group,
                group_count,
            } => self.repeat(node, *min, *max, *greedy, *first_group, *group_count, back),
            Node::Seq(items) => {
                if back {
                    for item in items.iter().rev() {
                        self.node(item, back);
                    }
                } else {
                    for item in items {
                        self.node(item, back);
                    }
                }
            }
            Node::Alt(alternatives) => self.alternation(alternatives, back),
        }
    }

    fn backref(&mut self, groups: Vec<u32>, ci: bool, back: bool) {
        self.backrefs.push(groups);
        let refs = (self.backrefs.len() - 1) as u32;
        self.emit(Inst::BackRef { refs, ci, back });
    }

    fn alternation(&mut self, alternatives: &[Node], back: bool) {
        let mut jumps = Vec::new();
        for (i, alternative) in alternatives.iter().enumerate() {
            if i + 1 == alternatives.len() {
                self.node(alternative, back);
                break;
            }
            let split = self.emit(Inst::Split {
                prefer: 0,
                other: 0,
            });
            let prefer = self.here();
            self.node(alternative, back);
            jumps.push(self.emit(Inst::Jmp(0)));
            let other = self.here();
            self.insts[split] = Inst::Split { prefer, other };
        }
        let end = self.here();
        for j in jumps {
            self.insts[j] = Inst::Jmp(end);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &mut self,
        node: &Node,
        min: u64,
        max: u64,
        greedy: bool,
        first_group: u32,
        group_count: u32,
        back: bool,
    ) {
        if max == 0 {
            return;
        }
        if !back
            && group_count == 0
            && let Some(atom) = self.atom(node)
        {
            self.emit(Inst::Simple {
                atom,
                min,
                max,
                greedy,
            });
            return;
        }
        let reg = self.reg();
        let pos_reg = self.reg();
        self.emit(Inst::RepeatInit { reg });
        let head = self.emit(Inst::RepeatBranch {
            reg,
            min,
            max,
            greedy,
            exit: 0,
        });
        self.emit(Inst::RepeatStart { pos_reg });
        if group_count > 0 {
            self.emit(Inst::ClearCaps {
                from: 2 * first_group,
                to: 2 * (first_group + group_count),
            });
        }
        self.node(node, back);
        self.emit(Inst::RepeatEnd {
            reg,
            pos_reg,
            min,
            head: head as u32,
        });
        let exit = self.here();
        if let Inst::RepeatBranch { exit: e, .. } = &mut self.insts[head] {
            *e = exit;
        }
    }
}
