//! Compiler from the AST to the bytecode of [`crate::bytecode`].
//!
//! Scope analysis. Every binding lives in an environment record: the
//! compiler keeps the stack of scopes of each function being compiled and
//! resolves an identifier either statically, to `(hops, slot)` from the
//! current environment, or dynamically by name when a `with` object or a
//! sloppy direct `eval` (which can add `var` bindings) lies between the
//! reference and its declaration, or when the name is not declared in any
//! compiled scope (global code, the code around an `eval`).
//!
//! `this`, `new.target` and the active function are read from the frame by
//! the function that owns them. Arrow functions and `eval` code reach them
//! through hidden bindings of the enclosing function environment, which the
//! compiler adds on first use (or up front for derived constructors and
//! functions that contain a direct `eval`).
//!
//! Completion values. Script and eval code keep the completion value of
//! the statement list in a frame register that statements update the way
//! UpdateEmpty does in the specification.
//!
//! Control flow. `break`, `continue` and `return` unwind the constructs
//! they leave (environments, exception handlers, iterators of `for-of`,
//! destructuring iterators) at compile time; when they leave a `try` with
//! `finally` (or a block with `using` declarations) they record the kind of
//! exit, jump into the `finally` code, and `EndFinally` jumps to a
//! trampoline compiled after it that continues the unwinding.

mod expr;
mod func;
mod pattern;
mod scan;
mod stmt;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::agent::Agent;
use crate::ast::{self, Program, Span};
use crate::bigint::BigInt;
use crate::bytecode::{
    Code, CodeKind, Declarations, EvalSite, FunctionFlags, Hidden, Op, SourceText, TemplateSite,
};
use crate::env::{BIND_CONST, BIND_SLOPPY_CONST, BIND_TDZ, ScopeNames};
use crate::gc::Atom;
use crate::value::Value;

pub(crate) use scan::{bound_names, lexical_declarations};

/// Name of the hidden binding of each [`Hidden`] value. `%` cannot start
/// an identifier, so they never collide with source names.
pub(crate) fn hidden_name(h: Hidden) -> &'static str {
    match h {
        Hidden::This => "%this",
        Hidden::NewTarget => "%newtarget",
        Hidden::Function => "%func",
    }
}

static NEXT_TEMPLATE_SITE: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Label(usize);

struct LabelData {
    target: Option<u32>,
    /// Instruction index and which of its targets refers to the label.
    refs: Vec<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    Function,
    Var,
    Lexical,
    Block,
    Catch,
    Class,
    With,
    FuncName,
    Eval,
}

pub(crate) struct Scope {
    pub kind: ScopeKind,
    /// The scope has an environment at run time.
    pub materialized: bool,
    pub names: Vec<String>,
    pub flags: Vec<u8>,
    /// Index of its layout in `Code::scopes`.
    pub index: Option<usize>,
    pub var_scope: bool,
    pub simple_catch: bool,
    /// Sloppy direct `eval` can add bindings here at run time.
    pub tainted: bool,
    pub hidden: [Option<u32>; 3],
}

impl Scope {
    pub fn new(kind: ScopeKind) -> Scope {
        Scope {
            kind,
            materialized: true,
            names: Vec::new(),
            flags: Vec::new(),
            index: None,
            var_scope: false,
            simple_catch: false,
            tainted: false,
            hidden: [None; 3],
        }
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// Adds a binding unless the name is already bound here; returns its
    /// slot.
    pub fn declare(&mut self, name: &str, flags: u8) -> u32 {
        if let Some(i) = self.find(name) {
            return i as u32;
        }
        self.names.push(name.to_string());
        self.flags.push(flags);
        (self.names.len() - 1) as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BreakKind {
    Loop,
    Switch,
    Label,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Exit {
    Return,
    Break(Option<String>),
    Continue(Option<String>),
}

pub(crate) struct FinallyInfo {
    pub kind_reg: u32,
    pub value_reg: u32,
    pub entry: Label,
    pub exits: Vec<Exit>,
}

pub(crate) enum Control {
    Breakable {
        labels: Vec<String>,
        kind: BreakKind,
        brk: Label,
        cont: Option<Label>,
        /// Iterator registers of a `for-of` (closed when an exit leaves the
        /// loop) and whether it is async.
        iter: Option<(u32, bool)>,
    },
    /// A pushed environment.
    Scope,
    /// An installed exception handler.
    Handler,
    /// Handler plus iterator of an array destructuring (a `yield` inside a
    /// default value can return through it).
    Destructure(u32),
    Finally(FinallyInfo),
}

/// Where an identifier lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    Local { hops: u32, slot: u32, flags: u8 },
    Dynamic,
}

/// What kind of function a [`FnState`] compiles.
#[derive(Clone, Debug, Default)]
pub(crate) struct FnInfo {
    pub flags: FunctionFlags,
    /// Class field initializer.
    pub field_init: bool,
    pub static_block: bool,
    /// The function has a home object (`super.x` allowed).
    pub method: bool,
}

pub(crate) struct FnState {
    pub kind: CodeKind,
    pub info: FnInfo,
    pub strict: bool,
    pub ops: Vec<Op>,
    pub consts: Vec<Value>,
    string_consts: HashMap<Vec<u16>, u32>,
    pub atoms: Vec<Atom>,
    atom_map: HashMap<String, u32>,
    pub functions: Vec<Rc<Code>>,
    pub scope_layouts: Vec<Option<Rc<ScopeNames>>>,
    pub exit_tables: Vec<Vec<u32>>,
    pub eval_sites: Vec<EvalSite>,
    pub decls: Vec<Declarations>,
    pub templates: Vec<TemplateSite>,
    pub arg_map: Vec<Option<u32>>,
    labels: Vec<LabelData>,
    pub scopes: Vec<Scope>,
    pub control: Vec<Control>,
    next_reg: u32,
    /// Track the completion value (script and eval code).
    pub completion: bool,
    /// Short-circuit trampolines of the open optional chains: label and
    /// number of values to pop.
    pub opt_chains: Vec<Vec<(Label, u32)>>,
    pub pending_labels: Vec<String>,
    /// Index in `scopes` of the function environment.
    pub fn_scope: Option<usize>,
    /// `this` lives in the hidden binding (derived constructors).
    pub this_in_slot: bool,
    /// Registers of the disposable resource stacks of the open `using`
    /// blocks.
    pub using_regs: Vec<u32>,
}

impl FnState {
    pub fn new(kind: CodeKind, info: FnInfo, strict: bool) -> FnState {
        FnState {
            kind,
            info,
            strict,
            ops: Vec::new(),
            consts: Vec::new(),
            string_consts: HashMap::new(),
            atoms: Vec::new(),
            atom_map: HashMap::new(),
            functions: Vec::new(),
            scope_layouts: Vec::new(),
            exit_tables: Vec::new(),
            eval_sites: Vec::new(),
            decls: Vec::new(),
            templates: Vec::new(),
            arg_map: Vec::new(),
            labels: Vec::new(),
            scopes: Vec::new(),
            control: Vec::new(),
            next_reg: 0,
            completion: false,
            opt_chains: Vec::new(),
            pending_labels: Vec::new(),
            fn_scope: None,
            this_in_slot: false,
            using_regs: Vec::new(),
        }
    }

    pub fn is_arrow(&self) -> bool {
        self.info.flags.is_arrow
    }
}

pub struct Compiler<'a> {
    pub(crate) agent: &'a mut Agent,
    pub(crate) source: Rc<str>,
    pub(crate) fns: Vec<FnState>,
}

impl<'a> Compiler<'a> {
    pub fn new(agent: &'a mut Agent, source: Rc<str>) -> Compiler<'a> {
        Compiler {
            agent,
            source,
            fns: Vec::new(),
        }
    }

    pub(crate) fn fs(&mut self) -> &mut FnState {
        self.fns.last_mut().expect("no function being compiled")
    }

    pub(crate) fn fr(&self) -> &FnState {
        self.fns.last().expect("no function being compiled")
    }

    pub(crate) fn strict(&self) -> bool {
        self.fr().strict
    }

    // ----- emission -----

    pub(crate) fn emit(&mut self, op: Op) -> usize {
        let fs = self.fs();
        fs.ops.push(op);
        fs.ops.len() - 1
    }

    pub(crate) fn pc(&self) -> u32 {
        self.fr().ops.len() as u32
    }

    pub(crate) fn new_label(&mut self) -> Label {
        let fs = self.fs();
        fs.labels.push(LabelData {
            target: None,
            refs: Vec::new(),
        });
        Label(fs.labels.len() - 1)
    }

    pub(crate) fn bind(&mut self, label: Label) {
        let fs = self.fs();
        let target = fs.ops.len() as u32;
        let data = &mut fs.labels[label.0];
        data.target = Some(target);
        let refs = std::mem::take(&mut data.refs);
        for (op, which) in refs {
            *fs.ops[op].targets_mut()[which] = target;
        }
    }

    fn link(&mut self, op: usize, which: usize, label: Label) {
        let fs = self.fs();
        match fs.labels[label.0].target {
            Some(t) => *fs.ops[op].targets_mut()[which] = t,
            None => fs.labels[label.0].refs.push((op, which)),
        }
    }

    /// Emits an instruction with one jump target.
    pub(crate) fn emit_jump(&mut self, op: Op, label: Label) -> usize {
        let i = self.emit(op);
        self.link(i, 0, label);
        i
    }

    /// Emits an instruction with two jump targets.
    pub(crate) fn emit_jump2(&mut self, op: Op, first: Label, second: Label) -> usize {
        let i = self.emit(op);
        self.link(i, 0, first);
        self.link(i, 1, second);
        i
    }

    pub(crate) fn jump(&mut self, label: Label) {
        self.emit_jump(Op::Jump(0), label);
    }

    pub(crate) fn alloc_reg(&mut self) -> u32 {
        let fs = self.fs();
        fs.next_reg += 1;
        fs.next_reg - 1
    }

    pub(crate) fn alloc_regs(&mut self, n: u32) -> u32 {
        let fs = self.fs();
        let first = fs.next_reg;
        fs.next_reg += n;
        first
    }

    // ----- constants -----

    pub(crate) fn atom(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.fr().atom_map.get(name) {
            return i;
        }
        let a = self.agent.heap.intern_str(name);
        let fs = self.fs();
        fs.atoms.push(a);
        fs.atom_map
            .insert(name.to_string(), (fs.atoms.len() - 1) as u32);
        (fs.atoms.len() - 1) as u32
    }

    pub(crate) fn string_const(&mut self, units: &[u16]) -> u32 {
        if let Some(&i) = self.fr().string_consts.get(units) {
            return i;
        }
        let s = self.agent.heap.alloc_string(units);
        let fs = self.fs();
        fs.consts.push(Value::String(s));
        let i = (fs.consts.len() - 1) as u32;
        fs.string_consts.insert(units.to_vec(), i);
        i
    }

    pub(crate) fn str_const(&mut self, s: &str) -> u32 {
        let units: Vec<u16> = s.encode_utf16().collect();
        self.string_const(&units)
    }

    pub(crate) fn emit_str(&mut self, s: &str) {
        let c = self.str_const(s);
        self.emit(Op::Const(c));
    }

    pub(crate) fn bigint_const(&mut self, digits: &str) -> u32 {
        let (text, radix) = match digits.get(..2) {
            Some("0x") | Some("0X") => (&digits[2..], 16),
            Some("0o") | Some("0O") => (&digits[2..], 8),
            Some("0b") | Some("0B") => (&digits[2..], 2),
            _ => (digits, 10),
        };
        let value = BigInt::parse_radix(text.as_bytes(), radix).unwrap_or_else(BigInt::zero);
        let b = self.agent.heap.alloc_bigint(value);
        let fs = self.fs();
        fs.consts.push(Value::BigInt(b));
        (fs.consts.len() - 1) as u32
    }

    pub(crate) fn throw_type_error(&mut self, message: &str) {
        let c = self.str_const(message);
        self.emit(Op::ThrowTypeError(c));
    }

    pub(crate) fn throw_reference_error(&mut self, message: &str) {
        let c = self.str_const(message);
        self.emit(Op::ThrowReferenceError(c));
    }

    pub(crate) fn new_template_site(&mut self, quasis: &[ast::TemplateElement]) -> u32 {
        let id = NEXT_TEMPLATE_SITE.fetch_add(1, Ordering::Relaxed);
        let mut cooked = Vec::new();
        let mut raw = Vec::new();
        for q in quasis {
            cooked.push(q.cooked.as_ref().map(|c| self.string_const(c)));
            let r: Vec<u16> = q.raw.encode_utf16().collect();
            raw.push(self.string_const(&r));
        }
        let fs = self.fs();
        fs.templates.push(TemplateSite { id, cooked, raw });
        (fs.templates.len() - 1) as u32
    }

    // ----- scopes -----

    /// Enters a scope; materialized scopes emit `PushScope` and a control
    /// entry so that exits pop it.
    pub(crate) fn push_scope(&mut self, mut scope: Scope) {
        if scope.materialized && scope.kind != ScopeKind::FuncName && scope.kind != ScopeKind::With
        {
            let fs = self.fs();
            fs.scope_layouts.push(None);
            let index = fs.scope_layouts.len() - 1;
            scope.index = Some(index);
            self.emit(Op::PushScope(index as u32));
        }
        let materialized = scope.materialized && scope.kind != ScopeKind::FuncName;
        self.fs().scopes.push(scope);
        if materialized {
            self.fs().control.push(Control::Scope);
        }
    }

    /// Leaves the innermost scope, emitting `PopScope` if `emit` is set.
    pub(crate) fn pop_scope(&mut self, emit: bool) {
        let scope = self.fs().scopes.pop().expect("scope stack underflow");
        if scope.materialized && scope.kind != ScopeKind::FuncName {
            let top = self.fs().control.pop();
            debug_assert!(matches!(top, Some(Control::Scope)));
            if emit {
                self.emit(Op::PopScope);
            }
        }
        self.finish_scope(&scope);
    }

    /// Stores the layout of a scope in the code block.
    pub(crate) fn finish_scope(&mut self, scope: &Scope) {
        let Some(index) = scope.index else {
            return;
        };
        let names = self.scope_names(scope);
        self.fs().scope_layouts[index] = Some(Rc::new(names));
    }

    pub(crate) fn scope_names(&mut self, scope: &Scope) -> ScopeNames {
        let names = scope
            .names
            .iter()
            .map(|n| self.agent.heap.intern_str(n))
            .collect();
        ScopeNames {
            names,
            flags: scope.flags.clone(),
            var_scope: scope.var_scope,
            simple_catch: scope.simple_catch,
            this_slot: scope.hidden[0],
            new_target_slot: scope.hidden[1],
            function_slot: scope.hidden[2],
        }
    }

    // ----- resolution -----

    /// Resolves an identifier from the current position.
    pub(crate) fn resolve(&self, name: &str) -> Binding {
        let mut hops = 0;
        let mut dynamic = false;
        for fs in self.fns.iter().rev() {
            for scope in fs.scopes.iter().rev() {
                if scope.kind == ScopeKind::With {
                    dynamic = true;
                    hops += 1;
                    continue;
                }
                if let Some(slot) = scope.find(name) {
                    if dynamic {
                        return Binding::Dynamic;
                    }
                    return Binding::Local {
                        hops,
                        slot: slot as u32,
                        flags: scope.flags[slot],
                    };
                }
                if scope.tainted {
                    dynamic = true;
                }
                if scope.materialized {
                    hops += 1;
                }
            }
        }
        Binding::Dynamic
    }

    /// Resolves a hidden binding statically, declaring it in the nearest
    /// non-arrow function environment; `None` means a dynamic lookup.
    pub(crate) fn resolve_hidden(&mut self, h: Hidden) -> Option<(u32, u32)> {
        let name = hidden_name(h);
        let mut hops = 0;
        let count = self.fns.len();
        for fi in (0..count).rev() {
            let fs = &self.fns[fi];
            if fs.kind != CodeKind::Function {
                return None;
            }
            if fs.is_arrow() {
                hops += fs.scopes.iter().filter(|s| s.materialized).count() as u32;
                continue;
            }
            let fn_scope = fs.fn_scope?;
            hops += fs.scopes[fn_scope + 1..]
                .iter()
                .filter(|s| s.materialized)
                .count() as u32;
            let fs = &mut self.fns[fi];
            let scope = &mut fs.scopes[fn_scope];
            let slot = match scope.find(name) {
                Some(slot) => slot as u32,
                None => {
                    let flags = if h == Hidden::This && fs.this_in_slot {
                        BIND_TDZ
                    } else {
                        0
                    };
                    scope.declare(name, flags)
                }
            };
            scope.hidden[h as usize] = Some(slot);
            return Some((hops, slot));
        }
        None
    }

    /// Pushes a hidden value (`this`, `new.target`, active function).
    pub(crate) fn load_hidden(&mut self, h: Hidden) {
        let own = {
            let fs = self.fr();
            fs.kind == CodeKind::Function && !fs.is_arrow()
        };
        if own && !(h == Hidden::This && self.fr().this_in_slot) {
            self.emit(match h {
                Hidden::This => Op::LoadThis,
                Hidden::NewTarget => Op::LoadNewTarget,
                Hidden::Function => Op::LoadFunction,
            });
            return;
        }
        if h == Hidden::This && self.fr().kind == CodeKind::Script {
            self.emit(Op::LoadThis);
            return;
        }
        match self.resolve_hidden(h) {
            Some((hops, slot)) => {
                self.emit(Op::GetLocal { hops, slot });
            }
            None => {
                self.emit(Op::GetHidden(h));
            }
        }
    }

    /// Reads an identifier.
    pub(crate) fn load_name(&mut self, name: &str) {
        match self.resolve(name) {
            Binding::Local { hops, slot, .. } => {
                self.emit(Op::GetLocal { hops, slot });
            }
            Binding::Dynamic => {
                let a = self.atom(name);
                self.emit(Op::GetName(a));
            }
        }
    }

    /// Initializes a binding with the value on the stack (pops it).
    pub(crate) fn init_name(&mut self, name: &str) {
        match self.resolve(name) {
            Binding::Local { hops, slot, .. } => {
                self.emit(Op::InitLocal { hops, slot });
            }
            Binding::Dynamic => {
                let a = self.atom(name);
                self.emit(Op::InitName(a));
            }
        }
    }

    // ----- control flow -----

    /// Emits the close of a `for-of` iterator on a normal exit.
    pub(crate) fn emit_iterator_close(&mut self, reg: u32, is_async: bool) {
        if is_async {
            let skip = self.new_label();
            self.emit_jump(Op::AsyncIteratorCloseCall { iter: reg, skip: 0 }, skip);
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
            let c = self.str_const("iterator return() result is not an object");
            self.emit(Op::CheckObject(c));
            self.emit(Op::Pop);
            self.bind(skip);
        } else {
            self.emit(Op::IteratorClose { iter: reg });
        }
    }

    fn snapshot_control(&self) -> Vec<ControlView> {
        self.fr()
            .control
            .iter()
            .map(|c| match c {
                Control::Breakable {
                    labels,
                    kind,
                    brk,
                    cont,
                    iter,
                } => ControlView::Breakable {
                    labels: labels.clone(),
                    kind: *kind,
                    brk: *brk,
                    cont: *cont,
                    iter: *iter,
                },
                Control::Scope => ControlView::Scope,
                Control::Handler => ControlView::Handler,
                Control::Destructure(b) => ControlView::Destructure(*b),
                Control::Finally(info) => ControlView::Finally {
                    kind_reg: info.kind_reg,
                    value_reg: info.value_reg,
                    entry: info.entry,
                },
            })
            .collect()
    }

    fn register_exit(&mut self, index: usize, exit: Exit) -> u32 {
        let Control::Finally(info) = &mut self.fs().control[index] else {
            unreachable!("not a finally entry");
        };
        let k = match info.exits.iter().position(|e| *e == exit) {
            Some(k) => k,
            None => {
                info.exits.push(exit);
                info.exits.len() - 1
            }
        };
        2 + k as u32
    }

    /// `break` (or `continue`) with an optional label. Returns false if no
    /// target was found (the parser rejects that).
    pub(crate) fn emit_break(&mut self, label: Option<&str>, is_continue: bool) -> bool {
        let view = self.snapshot_control();
        for (i, entry) in view.iter().enumerate().rev() {
            match entry {
                ControlView::Scope => {
                    self.emit(Op::PopScope);
                }
                ControlView::Handler => {
                    self.emit(Op::PopHandler);
                }
                ControlView::Destructure(base) => {
                    self.emit(Op::PopHandler);
                    self.emit(Op::DestructClose { base: *base });
                }
                ControlView::Finally {
                    kind_reg, entry, ..
                } => {
                    let exit = if is_continue {
                        Exit::Continue(label.map(str::to_string))
                    } else {
                        Exit::Break(label.map(str::to_string))
                    };
                    let k = self.register_exit(i, exit);
                    self.emit(Op::PopHandler);
                    self.emit(Op::Int(k as i32));
                    self.emit(Op::StoreReg(*kind_reg));
                    self.jump(*entry);
                    return true;
                }
                ControlView::Breakable {
                    labels,
                    kind,
                    brk,
                    cont,
                    iter,
                } => {
                    let matches = match label {
                        Some(l) => labels.iter().any(|x| x == l),
                        None => {
                            if is_continue {
                                *kind == BreakKind::Loop
                            } else {
                                *kind != BreakKind::Label
                            }
                        }
                    };
                    if matches && (!is_continue || *kind == BreakKind::Loop) {
                        if is_continue {
                            let target = cont.expect("loop has a continue label");
                            self.jump(target);
                        } else {
                            self.jump(*brk);
                        }
                        return true;
                    }
                    if let Some((reg, is_async)) = iter {
                        self.emit_iterator_close(*reg, *is_async);
                    }
                }
            }
        }
        false
    }

    /// Return with the value on the stack, unwinding every open construct.
    pub(crate) fn emit_return(&mut self) {
        let view = self.snapshot_control();
        for (i, entry) in view.iter().enumerate().rev() {
            match entry {
                ControlView::Scope => {
                    self.emit(Op::PopScope);
                }
                ControlView::Handler => {
                    self.emit(Op::PopHandler);
                }
                ControlView::Destructure(base) => {
                    self.emit(Op::PopHandler);
                    self.emit(Op::DestructClose { base: *base });
                }
                ControlView::Finally {
                    kind_reg,
                    value_reg,
                    entry,
                } => {
                    let k = self.register_exit(i, Exit::Return);
                    self.emit(Op::StoreReg(*value_reg));
                    self.emit(Op::PopHandler);
                    self.emit(Op::Int(k as i32));
                    self.emit(Op::StoreReg(*kind_reg));
                    self.jump(*entry);
                    return;
                }
                ControlView::Breakable { iter, .. } => {
                    if let Some((reg, is_async)) = iter {
                        self.emit_iterator_close(*reg, *is_async);
                    }
                }
            }
        }
        self.emit(Op::Return);
    }

    /// Compiles the trampolines of a finished `finally` (or disposal)
    /// block and stores their table; returns the table index.
    pub(crate) fn emit_exit_trampolines(&mut self, info: &FinallyInfo, table: u32) {
        let mut pcs = Vec::new();
        for exit in &info.exits {
            pcs.push(self.pc());
            match exit {
                Exit::Return => {
                    self.emit(Op::LoadReg(info.value_reg));
                    self.emit_return();
                }
                Exit::Break(l) => {
                    self.emit_break(l.as_deref(), false);
                }
                Exit::Continue(l) => {
                    self.emit_break(l.as_deref(), true);
                }
            }
        }
        self.fs().exit_tables[table as usize] = pcs;
    }

    pub(crate) fn new_exit_table(&mut self) -> u32 {
        let fs = self.fs();
        fs.exit_tables.push(Vec::new());
        (fs.exit_tables.len() - 1) as u32
    }

    // ----- finishing -----

    /// Turns the innermost function state into a code block.
    pub(crate) fn finish_fn(
        &mut self,
        name: Option<Atom>,
        length: u32,
        span: Option<Span>,
    ) -> Code {
        let mut fs = self.fns.pop().expect("no function being compiled");
        let scopes = fs
            .scope_layouts
            .drain(..)
            .map(|s| s.unwrap_or_else(|| Rc::new(ScopeNames::default())))
            .collect();
        Code {
            kind: fs.kind,
            ops: fs.ops,
            consts: fs.consts,
            atoms: fs.atoms,
            functions: fs.functions,
            scopes,
            exit_tables: fs.exit_tables,
            eval_sites: fs.eval_sites,
            decls: fs.decls,
            templates: fs.templates,
            arg_map: fs.arg_map,
            num_regs: fs.next_reg,
            strict: fs.strict,
            flags: fs.info.flags,
            name,
            name_scope: None,
            length,
            source: span.map(|span| SourceText {
                source: self.source.clone(),
                span,
            }),
        }
    }

    /// The facts a direct `eval` at the current position needs.
    pub(crate) fn eval_site(&mut self) -> u32 {
        let mut site = EvalSite {
            strict: self.strict(),
            ..EvalSite::default()
        };
        let mut found = false;
        for fs in self.fns.iter().rev() {
            if fs.kind != CodeKind::Function || fs.is_arrow() {
                continue;
            }
            site.new_target = true;
            site.super_property = fs.info.method;
            site.super_call = fs.info.flags.is_derived;
            site.class_field_initializer = fs.info.field_init;
            found = true;
            break;
        }
        for fs in &self.fns {
            for scope in &fs.scopes {
                if scope.kind == ScopeKind::Class {
                    for n in &scope.names {
                        if let Some(p) = n.strip_prefix('#') {
                            site.private_names.push(p.to_string());
                        }
                    }
                }
            }
        }
        // Inside an eval body, the facts of the code around the eval were
        // given to the parser; keep them.
        if let Some(ctx) = self.fns.first().and_then(outer_eval_context) {
            if !found {
                site.new_target = ctx.new_target;
                site.super_property = ctx.super_property;
                site.super_call = ctx.super_call;
                site.class_field_initializer = ctx.class_field_initializer;
            }
            site.private_names.extend(ctx.private_names.iter().cloned());
        }
        let fs = self.fs();
        fs.eval_sites.push(site);
        (fs.eval_sites.len() - 1) as u32
    }
}

/// The eval site facts an eval body was compiled with (stored as its first
/// eval site entry by [`compile_eval`]).
fn outer_eval_context(fs: &FnState) -> Option<EvalSite> {
    fs.eval_sites
        .first()
        .cloned()
        .filter(|_| fs.kind == CodeKind::Eval)
}

/// A copy of a control entry used while emitting unwinding code.
enum ControlView {
    Breakable {
        labels: Vec<String>,
        kind: BreakKind,
        brk: Label,
        cont: Option<Label>,
        iter: Option<(u32, bool)>,
    },
    Scope,
    Handler,
    Destructure(u32),
    Finally {
        kind_reg: u32,
        value_reg: u32,
        entry: Label,
    },
}

/// Flags of a lexical declaration binding.
pub(crate) fn lexical_flags(is_const: bool) -> u8 {
    if is_const {
        BIND_TDZ | BIND_CONST
    } else {
        BIND_TDZ
    }
}

pub(crate) const SLOPPY_CONST: u8 = BIND_SLOPPY_CONST;

/// Compiles a Script.
pub fn compile_script(agent: &mut Agent, program: &Program, source: Rc<str>) -> Rc<Code> {
    let mut c = Compiler::new(agent, source);
    let mut fs = FnState::new(CodeKind::Script, FnInfo::default(), program.strict);
    fs.completion = true;
    c.fns.push(fs);
    c.global_prologue(program);
    c.stmts(&program.body);
    c.emit(Op::LoadCompletion);
    c.emit(Op::Return);
    Rc::new(c.finish_fn(None, 0, None))
}

/// Compiles the body of an `eval`. `site` describes the caller (for a
/// direct eval) and is kept as eval site 0 so that nested direct evals
/// inherit it.
pub fn compile_eval(
    agent: &mut Agent,
    program: &Program,
    source: Rc<str>,
    site: EvalSite,
) -> Rc<Code> {
    let mut c = Compiler::new(agent, source);
    let mut fs = FnState::new(CodeKind::Eval, FnInfo::default(), program.strict);
    fs.completion = true;
    fs.eval_sites.push(site);
    c.fns.push(fs);
    c.eval_prologue(program);
    c.stmts(&program.body);
    c.emit(Op::LoadCompletion);
    c.emit(Op::Return);
    Rc::new(c.finish_fn(None, 0, None))
}
