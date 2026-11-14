//! Binding and assignment patterns, and stores to identifiers.

use crate::ast::{Expr, ExprKind, MemberProperty, Pattern, PatternElement, PatternKind};
use crate::bytecode::{Hidden, Op};
use crate::compiler::stmt::BindMode;
use crate::compiler::{Binding, Compiler, Control};
use crate::env::{BIND_CONST, BIND_SLOPPY_CONST};

/// A target whose reference was evaluated before its value.
pub(crate) enum Prepared<'p> {
    /// A static binding, or a dynamic one in `Init` mode.
    Ident(&'p str),
    /// A dynamic binding whose base is on the stack.
    DynamicIdent(&'p str),
    /// `[obj]`.
    Named(u32),
    /// `[obj key]`.
    Computed,
    /// `[obj name]`.
    Private,
    /// `[this base]`.
    SuperNamed(u32),
    /// `[this key base]`.
    SuperComputed,
    Pattern(&'p Pattern),
    /// A call expression (web-compatibility): ReferenceError on store.
    Invalid,
}

impl Compiler<'_> {
    /// Binds or assigns the value on the stack to the pattern (pops it).
    pub(crate) fn pattern(&mut self, p: &Pattern, mode: BindMode) {
        match &p.kind {
            PatternKind::Ident(name) => self.store_ident(name, mode),
            PatternKind::Expr(e) => {
                // The reference is evaluated after the value (for-in/of
                // heads): bring the value above it.
                let prepared = self.prepare_expr_target(e);
                match prepared {
                    Prepared::Named(_) | Prepared::DynamicIdent(_) => {
                        self.emit(Op::Swap);
                    }
                    Prepared::Computed | Prepared::Private | Prepared::SuperNamed(_) => {
                        self.emit(Op::RotUp3);
                        self.emit(Op::RotUp3);
                    }
                    Prepared::SuperComputed => {
                        self.emit(Op::RotUp4);
                        self.emit(Op::RotUp4);
                        self.emit(Op::RotUp4);
                    }
                    _ => {}
                }
                self.store_prepared(prepared, BindMode::Assign);
            }
            PatternKind::Array { elements, rest } => self.array_pattern(elements, rest, mode),
            PatternKind::Object { properties, rest } => self.object_pattern(properties, rest, mode),
        }
    }

    /// Stores to a binding, popping the value.
    pub(crate) fn store_ident(&mut self, name: &str, mode: BindMode) {
        if mode == BindMode::Init {
            self.init_name(name);
            return;
        }
        match self.resolve(name) {
            Binding::Local { .. } => {
                self.assign_local_value(name);
                self.emit(Op::Pop);
            }
            Binding::Dynamic => {
                let a = self.atom(name);
                self.emit(Op::ResolveRef(a));
                self.emit(Op::Swap);
                let strict = self.strict();
                self.emit(Op::SetRef { name: a, strict });
                self.emit(Op::Pop);
            }
        }
    }

    /// `[v] → [v]`: PutValue to a statically resolved binding.
    fn assign_local_value(&mut self, name: &str) {
        let Binding::Local { hops, slot, flags } = self.resolve(name) else {
            unreachable!("static binding");
        };
        if flags & BIND_CONST != 0 {
            self.emit(Op::ThrowConstAssign { hops, slot });
        } else if flags & BIND_SLOPPY_CONST != 0 {
            if self.strict() {
                self.emit(Op::ThrowConstAssign { hops, slot });
            }
        } else {
            self.emit(Op::SetLocal { hops, slot });
        }
    }

    /// `x = rhs`: resolves the reference, evaluates `rhs`, stores; leaves
    /// the value.
    pub(crate) fn assign_ident(&mut self, name: &str, rhs: impl FnOnce(&mut Self)) {
        match self.resolve(name) {
            Binding::Local { .. } => {
                rhs(self);
                self.assign_local_value(name);
            }
            Binding::Dynamic => {
                let a = self.atom(name);
                self.emit(Op::ResolveRef(a));
                rhs(self);
                let strict = self.strict();
                self.emit(Op::SetRef { name: a, strict });
            }
        }
    }

    /// Loads the private name `#name` (a Private Name symbol).
    pub(crate) fn load_private_name(&mut self, name: &str) {
        let key = format!("#{name}");
        self.load_name(&key);
    }

    /// Evaluates the reference part of a target expression.
    pub(crate) fn prepare_expr_target<'p>(&mut self, e: &'p Expr) -> Prepared<'p> {
        match &e.kind {
            ExprKind::Paren(inner) => self.prepare_expr_target(inner),
            ExprKind::Ident(name) => match self.resolve(name) {
                Binding::Local { .. } => Prepared::Ident(name),
                Binding::Dynamic => {
                    let a = self.atom(name);
                    self.emit(Op::ResolveRef(a));
                    Prepared::DynamicIdent(name)
                }
            },
            ExprKind::Member {
                object, property, ..
            } => {
                self.expr(object);
                match property {
                    MemberProperty::Name(n) => Prepared::Named(self.atom(&n.name)),
                    MemberProperty::Computed(k) => {
                        self.expr(k);
                        Prepared::Computed
                    }
                    MemberProperty::Private(n) => {
                        self.load_private_name(&n.name);
                        Prepared::Private
                    }
                }
            }
            ExprKind::SuperMember(property) => {
                self.load_hidden(Hidden::This);
                match property {
                    MemberProperty::Computed(k) => {
                        self.expr(k);
                        self.emit(Op::ToPropertyKey);
                        self.load_hidden(Hidden::Function);
                        self.emit(Op::SuperBase);
                        Prepared::SuperComputed
                    }
                    MemberProperty::Name(n) => {
                        self.load_hidden(Hidden::Function);
                        self.emit(Op::SuperBase);
                        Prepared::SuperNamed(self.atom(&n.name))
                    }
                    MemberProperty::Private(_) => Prepared::Invalid,
                }
            }
            _ => {
                self.expr(e);
                self.emit(Op::Pop);
                Prepared::Invalid
            }
        }
    }

    fn prepare_target<'p>(&mut self, p: &'p Pattern, mode: BindMode) -> Prepared<'p> {
        match &p.kind {
            PatternKind::Ident(name) => {
                if mode == BindMode::Assign {
                    if let Binding::Dynamic = self.resolve(name) {
                        let a = self.atom(name);
                        self.emit(Op::ResolveRef(a));
                        return Prepared::DynamicIdent(name);
                    }
                }
                Prepared::Ident(name)
            }
            PatternKind::Expr(e) => self.prepare_expr_target(e),
            _ => Prepared::Pattern(p),
        }
    }

    /// `[refs… v] → []`.
    pub(crate) fn store_prepared(&mut self, t: Prepared<'_>, mode: BindMode) {
        let strict = self.strict();
        match t {
            Prepared::Ident(name) => self.store_ident(name, mode),
            Prepared::DynamicIdent(name) => {
                let a = self.atom(name);
                self.emit(Op::SetRef { name: a, strict });
                self.emit(Op::Pop);
            }
            Prepared::Named(name) => {
                self.emit(Op::SetNamed { name, strict });
                self.emit(Op::Pop);
            }
            Prepared::Computed => {
                self.emit(Op::SetComputed { strict });
                self.emit(Op::Pop);
            }
            Prepared::Private => {
                self.emit(Op::SetPrivate);
                self.emit(Op::Pop);
            }
            Prepared::SuperNamed(name) => {
                self.emit(Op::SetSuperNamed { name, strict });
                self.emit(Op::Pop);
            }
            Prepared::SuperComputed => {
                self.emit(Op::SetSuperComputed { strict });
                self.emit(Op::Pop);
            }
            Prepared::Pattern(p) => self.pattern(p, mode),
            Prepared::Invalid => {
                self.emit(Op::Pop);
                self.throw_reference_error("invalid assignment target");
            }
        }
    }

    /// The name NamedEvaluation gives to a default value of this target.
    fn target_name(p: &Pattern) -> Option<&str> {
        match &p.kind {
            PatternKind::Ident(n) => Some(n),
            _ => None,
        }
    }

    /// `[v] → [v']`: replaces `undefined` by the default value.
    fn element_default(&mut self, element: &PatternElement) {
        let Some(default) = &element.default else {
            return;
        };
        let skip = self.new_label();
        self.emit_jump(Op::JumpIfNotUndefined(0), skip);
        self.emit(Op::Pop);
        match Self::target_name(&element.target) {
            Some(name) => {
                let name = name.to_string();
                self.expr_named(default, &name);
            }
            None => self.expr(default),
        }
        self.bind(skip);
    }

    fn array_pattern(
        &mut self,
        elements: &[Option<PatternElement>],
        rest: &Option<Box<Pattern>>,
        mode: BindMode,
    ) {
        let base = self.alloc_regs(3);
        self.emit(Op::DestructIter { base });
        let (on_throw, end) = (self.new_label(), self.new_label());
        self.emit_jump(Op::PushHandler(0), on_throw);
        self.fs().control.push(Control::Destructure(base));
        for element in elements {
            match element {
                None => {
                    self.emit(Op::DestructStep { base });
                    self.emit(Op::Pop);
                }
                Some(el) => {
                    let t = self.prepare_target(&el.target, mode);
                    self.emit(Op::DestructStep { base });
                    self.element_default(el);
                    self.store_prepared(t, mode);
                }
            }
        }
        if let Some(r) = rest {
            let t = self.prepare_target(r, mode);
            self.emit(Op::DestructRest { base });
            self.store_prepared(t, mode);
        }
        self.fs().control.pop();
        self.emit(Op::PopHandler);
        self.emit(Op::DestructClose { base });
        self.jump(end);
        self.bind(on_throw);
        self.emit(Op::DestructCloseThrow { base });
        self.bind(end);
    }

    fn object_pattern(
        &mut self,
        properties: &[crate::ast::PatternProperty],
        rest: &Option<Box<Pattern>>,
        mode: BindMode,
    ) {
        self.emit(Op::RequireObjectCoercible);
        let src = self.alloc_reg();
        self.emit(Op::StoreReg(src));
        let excluded = if rest.is_some() {
            let r = self.alloc_reg();
            self.emit(Op::NewArray);
            self.emit(Op::StoreReg(r));
            Some(r)
        } else {
            None
        };
        for prop in properties {
            self.property_key(&prop.key);
            let key = self.alloc_reg();
            self.emit(Op::StoreReg(key));
            if let Some(ex) = excluded {
                self.emit(Op::LoadReg(ex));
                self.emit(Op::LoadReg(key));
                self.emit(Op::ArrayPush);
                self.emit(Op::Pop);
            }
            let t = self.prepare_target(&prop.value.target, mode);
            self.emit(Op::LoadReg(src));
            self.emit(Op::LoadReg(key));
            self.emit(Op::GetComputed);
            self.element_default(&prop.value);
            self.store_prepared(t, mode);
        }
        if let (Some(r), Some(ex)) = (rest, excluded) {
            let t = self.prepare_target(r, mode);
            self.emit(Op::LoadReg(src));
            self.emit(Op::LoadReg(ex));
            self.emit(Op::CopyRest);
            self.store_prepared(t, mode);
        }
    }
}
