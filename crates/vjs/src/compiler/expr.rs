//! Expressions.

use crate::ast::{
    Argument, ArrayElement, AssignOp, BinaryOp, Expr, ExprKind, LogicalOp, MemberProperty,
    MethodKind, Pattern, PatternKind, Property, PropertyKey, PropertyKind, Template, UnaryOp,
    UpdateOp,
};
use crate::bytecode::{ClassName, Hidden, MethodOp, Op};
use crate::compiler::func::FnName;
use crate::compiler::pattern::Prepared;
use crate::compiler::stmt::BindMode;
use crate::compiler::{Binding, Compiler};
use crate::number::number_to_string;

pub(crate) fn method_op(kind: MethodKind) -> MethodOp {
    match kind {
        MethodKind::Method => MethodOp::Method,
        MethodKind::Get => MethodOp::Get,
        MethodKind::Set => MethodOp::Set,
    }
}

fn binary_op(op: BinaryOp) -> Op {
    match op {
        BinaryOp::Add => Op::Add,
        BinaryOp::Sub => Op::Sub,
        BinaryOp::Mul => Op::Mul,
        BinaryOp::Div => Op::Div,
        BinaryOp::Rem => Op::Rem,
        BinaryOp::Exp => Op::Exp,
        BinaryOp::Shl => Op::Shl,
        BinaryOp::Shr => Op::Shr,
        BinaryOp::UShr => Op::UShr,
        BinaryOp::BitAnd => Op::BitAnd,
        BinaryOp::BitOr => Op::BitOr,
        BinaryOp::BitXor => Op::BitXor,
        BinaryOp::Eq => Op::Eq,
        BinaryOp::NotEq => Op::NotEq,
        BinaryOp::StrictEq => Op::StrictEq,
        BinaryOp::StrictNotEq => Op::StrictNotEq,
        BinaryOp::Lt => Op::Lt,
        BinaryOp::Gt => Op::Gt,
        BinaryOp::LtEq => Op::LtEq,
        BinaryOp::GtEq => Op::GtEq,
        BinaryOp::In => Op::In,
        BinaryOp::InstanceOf => Op::InstanceOf,
    }
}

fn assign_binary(op: AssignOp) -> Option<Op> {
    Some(match op {
        AssignOp::Add => Op::Add,
        AssignOp::Sub => Op::Sub,
        AssignOp::Mul => Op::Mul,
        AssignOp::Div => Op::Div,
        AssignOp::Rem => Op::Rem,
        AssignOp::Exp => Op::Exp,
        AssignOp::Shl => Op::Shl,
        AssignOp::Shr => Op::Shr,
        AssignOp::UShr => Op::UShr,
        AssignOp::BitAnd => Op::BitAnd,
        AssignOp::BitOr => Op::BitOr,
        AssignOp::BitXor => Op::BitXor,
        _ => return None,
    })
}

/// IsAnonymousFunctionDefinition (through parentheses).
pub(crate) fn is_anonymous_function(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Paren(inner) => is_anonymous_function(inner),
        ExprKind::Function(f) => f.name.is_none(),
        ExprKind::Arrow(_) => true,
        ExprKind::Class(c) => c.name.is_none(),
        _ => false,
    }
}

/// The name of a non-computed property key (for NamedEvaluation).
pub(crate) fn static_key_name(key: &PropertyKey) -> Option<String> {
    match key {
        PropertyKey::Name(n) => Some(n.clone()),
        PropertyKey::String(u) => Some(String::from_utf16_lossy(u)),
        PropertyKey::Number(n) => Some(number_to_string(*n)),
        PropertyKey::BigInt(d) => {
            let (text, radix) = match d.get(..2) {
                Some("0x") | Some("0X") => (&d[2..], 16),
                Some("0o") | Some("0O") => (&d[2..], 8),
                Some("0b") | Some("0B") => (&d[2..], 2),
                _ => (d.as_str(), 10),
            };
            crate::bigint::BigInt::parse_radix(text.as_bytes(), radix)
                .map(|b| b.to_string_radix(10))
        }
        PropertyKey::Private(n) => Some(format!("#{n}")),
        PropertyKey::Computed(_) => None,
    }
}

impl Compiler<'_> {
    pub(crate) fn number(&mut self, n: f64) {
        if n.fract() == 0.0 && n.abs() < 2_147_483_648.0 && !(n == 0.0 && n.is_sign_negative()) {
            self.emit(Op::Int(n as i32));
        } else {
            self.emit(Op::Num(n));
        }
    }

    /// Pushes a property key value (ToPropertyKey applied).
    pub(crate) fn property_key(&mut self, key: &PropertyKey) {
        match key {
            PropertyKey::Name(n) => self.emit_str(n),
            PropertyKey::String(u) => {
                let c = self.string_const(u);
                self.emit(Op::Const(c));
            }
            PropertyKey::Number(n) => {
                self.number(*n);
                self.emit(Op::ToPropertyKey);
            }
            PropertyKey::BigInt(d) => {
                let c = self.bigint_const(d);
                self.emit(Op::Const(c));
                self.emit(Op::ToPropertyKey);
            }
            PropertyKey::Computed(e) => {
                self.expr(e);
                self.emit(Op::ToPropertyKey);
            }
            PropertyKey::Private(n) => self.load_private_name(n),
        }
    }

    /// NamedEvaluation of `e` with a static name.
    pub(crate) fn expr_named(&mut self, e: &Expr, name: &str) {
        match &e.kind {
            ExprKind::Paren(inner) if is_anonymous_function(inner) => self.expr_named(inner, name),
            ExprKind::Function(f) if f.name.is_none() => {
                let idx = self.function(f, FnName::Static(name.to_string()), false);
                self.emit(Op::Closure(idx));
            }
            ExprKind::Arrow(f) => {
                let idx = self.function(f, FnName::Static(name.to_string()), false);
                self.emit(Op::Closure(idx));
            }
            ExprKind::Class(c) if c.name.is_none() => {
                let a = self.atom(name);
                self.class_with(c, ClassName::Atom(a));
            }
            _ => self.expr(e),
        }
    }

    /// `[key] → [key v]`: NamedEvaluation with the property key on the
    /// stack (computed keys).
    pub(crate) fn expr_keyed(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Paren(inner) if is_anonymous_function(inner) => self.expr_keyed(inner),
            ExprKind::Function(f) if f.name.is_none() => {
                let idx = self.function(f, FnName::Keyed, false);
                self.emit(Op::ClosureKeyName(idx));
            }
            ExprKind::Arrow(f) => {
                let idx = self.function(f, FnName::Keyed, false);
                self.emit(Op::ClosureKeyName(idx));
            }
            ExprKind::Class(c) if c.name.is_none() => {
                let r = self.alloc_reg();
                self.emit(Op::Dup);
                self.emit(Op::StoreReg(r));
                self.class_with(c, ClassName::Reg(r));
            }
            _ => self.expr(e),
        }
    }

    pub(crate) fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Ident(name) => self.load_name(name),
            ExprKind::This => self.load_hidden(Hidden::This),
            ExprKind::Null => {
                self.emit(Op::Null);
            }
            ExprKind::Bool(b) => {
                self.emit(if *b { Op::True } else { Op::False });
            }
            ExprKind::Number(n) => self.number(*n),
            ExprKind::BigInt(d) => {
                let c = self.bigint_const(d);
                self.emit(Op::Const(c));
            }
            ExprKind::String(u) => {
                let c = self.string_const(u);
                self.emit(Op::Const(c));
            }
            ExprKind::RegExp { pattern, flags } => {
                let pattern = self.str_const(pattern);
                let flags = self.str_const(flags);
                self.emit(Op::RegExp { pattern, flags });
            }
            ExprKind::Template(t) => self.template(t),
            ExprKind::TaggedTemplate { tag, quasi } => {
                self.callee(tag);
                let site = self.new_template_site(&quasi.quasis);
                self.emit(Op::GetTemplateObject(site));
                for x in &quasi.expressions {
                    self.expr(x);
                }
                self.emit(Op::Call(1 + quasi.expressions.len() as u32));
            }
            ExprKind::Array(elements) => {
                self.emit(Op::NewArray);
                for el in elements {
                    match el {
                        ArrayElement::Hole => {
                            self.emit(Op::ArrayHole);
                        }
                        ArrayElement::Expr(x) => {
                            self.expr(x);
                            self.emit(Op::ArrayPush);
                        }
                        ArrayElement::Spread(x) => {
                            self.expr(x);
                            self.emit(Op::ArraySpread);
                        }
                    }
                }
            }
            ExprKind::Object(props) => self.object_literal(props),
            ExprKind::Function(f) => {
                if f.name.is_some() {
                    let idx = self.function(f, FnName::Own, true);
                    self.emit(Op::ClosureNamed(idx));
                } else {
                    let idx = self.function(f, FnName::Static(String::new()), false);
                    self.emit(Op::Closure(idx));
                }
            }
            ExprKind::Arrow(f) => {
                let idx = self.function(f, FnName::Static(String::new()), false);
                self.emit(Op::Closure(idx));
            }
            ExprKind::Class(c) => self.class(c, None),
            ExprKind::Paren(inner) => self.expr(inner),
            ExprKind::Unary { op, argument } => self.unary(*op, argument),
            ExprKind::Update {
                op,
                prefix,
                argument,
            } => self.update(*op, *prefix, argument),
            ExprKind::Binary { op, left, right } => {
                self.expr(left);
                self.expr(right);
                self.emit(binary_op(*op));
            }
            ExprKind::Logical { op, left, right } => {
                self.expr(left);
                let end = self.new_label();
                let jump = match op {
                    LogicalOp::And => Op::JumpIfFalseKeep(0),
                    LogicalOp::Or => Op::JumpIfTrueKeep(0),
                    LogicalOp::Coalesce => Op::JumpIfNotNullishKeep(0),
                };
                self.emit_jump(jump, end);
                self.expr(right);
                self.bind(end);
            }
            ExprKind::PrivateIn { name, right } => {
                self.load_private_name(&name.name);
                self.expr(right);
                self.emit(Op::PrivateIn);
            }
            ExprKind::Assign { op, target, value } => self.assign(*op, target, value),
            ExprKind::Conditional {
                test,
                consequent,
                alternate,
            } => {
                self.expr(test);
                let (alt, end) = (self.new_label(), self.new_label());
                self.emit_jump(Op::JumpIfFalse(0), alt);
                self.expr(consequent);
                self.jump(end);
                self.bind(alt);
                self.expr(alternate);
                self.bind(end);
            }
            ExprKind::Call {
                callee,
                arguments,
                optional,
            } => self.call(callee, arguments, *optional),
            ExprKind::SuperCall(arguments) => self.super_call(arguments),
            ExprKind::New { callee, arguments } => {
                self.expr(callee);
                match self.arguments(arguments) {
                    Some(argc) => self.emit(Op::New(argc)),
                    None => self.emit(Op::NewSpread),
                };
            }
            ExprKind::Member {
                object,
                property,
                optional,
            } => {
                self.expr(object);
                if *optional {
                    self.optional_check(0, 1);
                }
                match property {
                    MemberProperty::Name(n) => {
                        let a = self.atom(&n.name);
                        self.emit(Op::GetNamed(a));
                    }
                    MemberProperty::Computed(k) => {
                        self.expr(k);
                        self.emit(Op::GetComputed);
                    }
                    MemberProperty::Private(n) => {
                        self.load_private_name(&n.name);
                        self.emit(Op::GetPrivate);
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
                        self.emit(Op::GetSuperComputed);
                    }
                    MemberProperty::Name(n) => {
                        self.load_hidden(Hidden::Function);
                        self.emit(Op::SuperBase);
                        let a = self.atom(&n.name);
                        self.emit(Op::GetSuperNamed(a));
                    }
                    MemberProperty::Private(_) => {
                        self.throw_type_error("invalid super property");
                    }
                }
            }
            ExprKind::OptionalChain(inner) => self.optional_chain(Op::Undefined, |c| c.expr(inner)),
            ExprKind::Sequence(list) => {
                for (i, x) in list.iter().enumerate() {
                    self.expr(x);
                    if i + 1 < list.len() {
                        self.emit(Op::Pop);
                    }
                }
            }
            ExprKind::Yield { argument, delegate } => {
                if *delegate {
                    self.yield_star(argument.as_deref());
                } else {
                    self.yield_expr(argument.as_deref());
                }
            }
            ExprKind::Await(x) => {
                self.expr(x);
                self.emit(Op::Await);
                self.emit(Op::AwaitResume);
            }
            ExprKind::NewTarget => self.load_hidden(Hidden::NewTarget),
            ExprKind::ImportMeta => {
                self.throw_type_error("import.meta is only available in module code");
            }
            ExprKind::ImportCall { specifier, options } => {
                self.expr(specifier);
                self.emit(Op::Pop);
                if let Some(o) = options {
                    self.expr(o);
                    self.emit(Op::Pop);
                }
                self.throw_type_error("dynamic import is not available");
            }
        }
    }

    fn template(&mut self, t: &Template) {
        let first = t.quasis[0].cooked.clone().unwrap_or_default();
        let c = self.string_const(&first);
        self.emit(Op::Const(c));
        for (i, x) in t.expressions.iter().enumerate() {
            self.expr(x);
            self.emit(Op::ToStringOp);
            self.emit(Op::Concat);
            let q = t.quasis[i + 1].cooked.clone().unwrap_or_default();
            if !q.is_empty() {
                let c = self.string_const(&q);
                self.emit(Op::Const(c));
                self.emit(Op::Concat);
            }
        }
    }

    /// Registers a short-circuit point of the innermost optional chain:
    /// jumps if the value `depth` entries down is nullish, popping `pops`
    /// values.
    pub(crate) fn optional_check(&mut self, depth: u32, pops: u32) {
        let label = self.new_label();
        self.emit_jump(Op::JumpIfNullishAt { depth, target: 0 }, label);
        if let Some(list) = self.fs().opt_chains.last_mut() {
            list.push((label, pops));
        }
    }

    /// An optional chain: `short` is pushed when it short-circuits.
    pub(crate) fn optional_chain(&mut self, short: Op, body: impl FnOnce(&mut Self)) {
        self.fs().opt_chains.push(Vec::new());
        body(self);
        let sites = self.fs().opt_chains.pop().unwrap_or_default();
        if sites.is_empty() {
            return;
        }
        let end = self.new_label();
        self.jump(end);
        for (label, pops) in sites {
            self.bind(label);
            self.emit(Op::PopN(pops));
            self.emit(short);
            self.jump(end);
        }
        self.bind(end);
    }

    /// Argument list: `Some(argc)` with the values pushed, or `None` with
    /// an array of them (spread).
    pub(crate) fn arguments(&mut self, args: &[Argument]) -> Option<u32> {
        if args.iter().any(|a| matches!(a, Argument::Spread(_))) {
            self.emit(Op::NewArray);
            for a in args {
                match a {
                    Argument::Expr(x) => {
                        self.expr(x);
                        self.emit(Op::ArrayPush);
                    }
                    Argument::Spread(x) => {
                        self.expr(x);
                        self.emit(Op::ArraySpread);
                    }
                }
            }
            return None;
        }
        for a in args {
            if let Argument::Expr(x) = a {
                self.expr(x);
            }
        }
        Some(args.len() as u32)
    }

    /// Pushes `[f this]` for a call of `callee`.
    fn callee(&mut self, callee: &Expr) {
        match &callee.kind {
            ExprKind::Paren(inner)
                if matches!(
                    inner.kind,
                    ExprKind::Member { .. } | ExprKind::SuperMember(_) | ExprKind::Ident(_)
                ) =>
            {
                self.callee(inner)
            }
            ExprKind::Ident(name) => match self.resolve(name) {
                Binding::Local { hops, slot, .. } => {
                    self.emit(Op::GetLocal { hops, slot });
                    self.emit(Op::Undefined);
                }
                Binding::Dynamic => {
                    let a = self.atom(name);
                    self.emit(Op::GetNameWithThis(a));
                }
            },
            ExprKind::Member {
                object,
                property,
                optional,
            } => {
                self.expr(object);
                if *optional {
                    self.optional_check(0, 1);
                }
                match property {
                    MemberProperty::Name(n) => {
                        let a = self.atom(&n.name);
                        self.emit(Op::GetMethodNamed(a));
                    }
                    MemberProperty::Computed(k) => {
                        self.expr(k);
                        self.emit(Op::GetMethodComputed);
                    }
                    MemberProperty::Private(n) => {
                        self.load_private_name(&n.name);
                        self.emit(Op::GetPrivateMethod);
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
                        self.emit(Op::GetSuperMethodComputed);
                    }
                    MemberProperty::Name(n) => {
                        self.load_hidden(Hidden::Function);
                        self.emit(Op::SuperBase);
                        let a = self.atom(&n.name);
                        self.emit(Op::GetSuperMethodNamed(a));
                    }
                    MemberProperty::Private(_) => {
                        self.throw_type_error("invalid super property");
                    }
                }
            }
            _ => {
                self.expr(callee);
                self.emit(Op::Undefined);
            }
        }
    }

    fn is_eval_callee(callee: &Expr) -> bool {
        match &callee.kind {
            ExprKind::Ident(n) => n == "eval",
            ExprKind::Paren(inner) => Self::is_eval_callee(inner),
            _ => false,
        }
    }

    fn call(&mut self, callee: &Expr, args: &[Argument], optional: bool) {
        self.callee(callee);
        if optional {
            self.optional_check(1, 2);
        }
        let direct_eval = Self::is_eval_callee(callee);
        let argc = self.arguments(args);
        if direct_eval {
            let site = self.eval_site();
            match argc {
                Some(argc) => self.emit(Op::CallEval { argc, site }),
                None => self.emit(Op::CallEvalArray { site }),
            };
            return;
        }
        match argc {
            Some(argc) => self.emit(Op::Call(argc)),
            None => self.emit(Op::CallArray),
        };
    }

    fn super_call(&mut self, args: &[Argument]) {
        self.load_hidden(Hidden::Function);
        self.emit(Op::GetSuperConstructor);
        self.load_hidden(Hidden::NewTarget);
        match self.arguments(args) {
            Some(argc) => self.emit(Op::SuperCall(argc)),
            None => self.emit(Op::SuperCallSpread),
        };
        match self.resolve_hidden(Hidden::This) {
            Some((hops, slot)) => self.emit(Op::BindThis { hops, slot }),
            None => self.emit(Op::BindThisDyn),
        };
        self.load_hidden(Hidden::Function);
        self.emit(Op::InitInstanceElements);
    }

    fn object_literal(&mut self, props: &[Property]) {
        self.emit(Op::NewObject);
        for prop in props {
            match &prop.kind {
                PropertyKind::Init { key, value, .. } => {
                    self.property_key(key);
                    if is_anonymous_function(value) {
                        match static_key_name(key) {
                            Some(name) => self.expr_named(value, &name),
                            None => self.expr_keyed(value),
                        }
                    } else {
                        self.expr(value);
                    }
                    self.emit(Op::DefineField);
                }
                PropertyKind::Proto(value) => {
                    self.expr(value);
                    self.emit(Op::SetProtoLiteral);
                }
                PropertyKind::Method {
                    key,
                    kind,
                    function,
                } => {
                    self.property_key(key);
                    let func = self.method_function(function);
                    self.emit(Op::DefineMethod {
                        func,
                        kind: method_op(*kind),
                    });
                }
                PropertyKind::Spread(x) => {
                    self.expr(x);
                    self.emit(Op::CopyDataProps);
                }
            }
        }
    }

    fn unary(&mut self, op: UnaryOp, argument: &Expr) {
        match op {
            UnaryOp::Typeof => {
                let mut arg = argument;
                while let ExprKind::Paren(inner) = &arg.kind {
                    arg = inner;
                }
                if let ExprKind::Ident(name) = &arg.kind {
                    if let Binding::Dynamic = self.resolve(name) {
                        let a = self.atom(name);
                        self.emit(Op::GetNameOrUndefined(a));
                        self.emit(Op::Typeof);
                        return;
                    }
                }
                self.expr(argument);
                self.emit(Op::Typeof);
            }
            UnaryOp::Delete => self.delete(argument),
            UnaryOp::Void => {
                self.expr(argument);
                self.emit(Op::Pop);
                self.emit(Op::Undefined);
            }
            UnaryOp::Not => {
                self.expr(argument);
                self.emit(Op::Not);
            }
            UnaryOp::Minus => {
                self.expr(argument);
                self.emit(Op::Neg);
            }
            UnaryOp::Plus => {
                self.expr(argument);
                self.emit(Op::ToNumber);
            }
            UnaryOp::BitNot => {
                self.expr(argument);
                self.emit(Op::BitNot);
            }
        }
    }

    fn delete(&mut self, argument: &Expr) {
        let strict = self.strict();
        match &argument.kind {
            ExprKind::Paren(inner) => self.delete(inner),
            ExprKind::Ident(name) => match self.resolve(name) {
                Binding::Local { .. } => {
                    self.emit(Op::False);
                }
                Binding::Dynamic => {
                    let a = self.atom(name);
                    self.emit(Op::DeleteName(a));
                }
            },
            ExprKind::Member {
                object,
                property,
                optional,
            } => {
                self.expr(object);
                if *optional {
                    self.optional_check(0, 1);
                }
                match property {
                    MemberProperty::Name(n) => {
                        let name = self.atom(&n.name);
                        self.emit(Op::DeleteNamed { name, strict });
                    }
                    MemberProperty::Computed(k) => {
                        self.expr(k);
                        self.emit(Op::DeleteComputed { strict });
                    }
                    MemberProperty::Private(_) => {
                        self.emit(Op::Pop);
                        self.throw_type_error("private members cannot be deleted");
                    }
                }
            }
            ExprKind::SuperMember(property) => {
                self.load_hidden(Hidden::This);
                self.emit(Op::Pop);
                if let MemberProperty::Computed(k) = property {
                    self.expr(k);
                    self.emit(Op::Pop);
                }
                self.throw_reference_error("unsupported reference to 'super'");
            }
            ExprKind::OptionalChain(inner) => {
                self.optional_chain(Op::True, |c| c.delete(inner));
            }
            _ => {
                self.expr(argument);
                self.emit(Op::Pop);
                self.emit(Op::True);
            }
        }
    }

    /// `x++`, `--o.p`…: `[] → [result]`.
    fn update(&mut self, op: UpdateOp, prefix: bool, argument: &Expr) {
        let step = if op == UpdateOp::Increment {
            Op::Inc
        } else {
            Op::Dec
        };
        let mut arg = argument;
        while let ExprKind::Paren(inner) = &arg.kind {
            arg = inner;
        }
        if let ExprKind::Ident(name) = &arg.kind {
            if let Binding::Local { hops, slot, .. } = self.resolve(name) {
                self.emit(Op::GetLocal { hops, slot });
                self.emit(Op::ToNumeric);
                if !prefix {
                    self.emit(Op::Dup);
                }
                self.emit(step);
                self.assign_local_value_pub(name);
                if !prefix {
                    self.emit(Op::Pop);
                }
                return;
            }
        }
        let prepared = self.prepare_expr_target(arg);
        // `[refs] → [refs old]`
        let depth = match &prepared {
            Prepared::DynamicIdent(name) => {
                let a = self.atom(name);
                self.emit(Op::GetRef(a));
                1
            }
            Prepared::Named(a) => {
                self.emit(Op::GetNamedKeep(*a));
                1
            }
            Prepared::Computed => {
                self.emit(Op::GetComputedKeep);
                2
            }
            Prepared::Private => {
                self.emit(Op::GetPrivateKeep);
                2
            }
            Prepared::SuperNamed(a) => {
                self.emit(Op::Dup2);
                self.emit(Op::GetSuperNamed(*a));
                2
            }
            Prepared::SuperComputed => {
                self.super_computed_keep();
                3
            }
            _ => {
                self.emit(Op::Undefined);
                self.store_prepared(Prepared::Invalid, BindMode::Assign);
                self.emit(Op::Undefined);
                return;
            }
        };
        self.emit(Op::ToNumeric);
        if !prefix {
            self.emit(Op::Dup);
            match depth {
                1 => self.emit(Op::RotUp3),
                2 => self.emit(Op::RotUp4),
                _ => {
                    // [this key base old old] → keep old in a register.
                    let r = self.alloc_reg();
                    self.emit(Op::StoreReg(r));
                    self.emit(step);
                    self.store_keep(prepared);
                    self.emit(Op::Pop);
                    self.emit(Op::LoadReg(r));
                    return;
                }
            };
        }
        self.emit(step);
        self.store_keep(prepared);
        if !prefix {
            self.emit(Op::Pop);
        }
    }

    /// `[this key base] → [this key base old]`.
    fn super_computed_keep(&mut self) {
        let (rt, rk, rb) = (self.alloc_reg(), self.alloc_reg(), self.alloc_reg());
        self.emit(Op::StoreReg(rb));
        self.emit(Op::StoreReg(rk));
        self.emit(Op::StoreReg(rt));
        for r in [rt, rk, rb, rt, rk, rb] {
            self.emit(Op::LoadReg(r));
        }
        self.emit(Op::GetSuperComputed);
    }

    /// `[refs v] → [v]`: the store of a compound assignment.
    fn store_keep(&mut self, prepared: Prepared<'_>) {
        let strict = self.strict();
        match prepared {
            Prepared::Ident(name) => self.assign_local_value_pub(name),
            Prepared::DynamicIdent(name) => {
                let a = self.atom(name);
                self.emit(Op::SetRef { name: a, strict });
            }
            Prepared::Named(name) => {
                self.emit(Op::SetNamed { name, strict });
            }
            Prepared::Computed => {
                self.emit(Op::SetComputed { strict });
            }
            Prepared::Private => {
                self.emit(Op::SetPrivate);
            }
            Prepared::SuperNamed(name) => {
                self.emit(Op::SetSuperNamed { name, strict });
            }
            Prepared::SuperComputed => {
                self.emit(Op::SetSuperComputed { strict });
            }
            Prepared::Pattern(_) | Prepared::Invalid => {
                self.emit(Op::Pop);
                self.throw_reference_error("invalid assignment target");
            }
        }
    }

    fn assign(&mut self, op: AssignOp, target: &Pattern, value: &Expr) {
        if op == AssignOp::Assign {
            match &target.kind {
                PatternKind::Ident(name) => {
                    self.assign_ident(name, |c| c.expr_named(value, name));
                }
                PatternKind::Expr(e) => {
                    let prepared = self.prepare_expr_target(e);
                    let name = match &prepared {
                        Prepared::Ident(n) | Prepared::DynamicIdent(n) => Some(n.to_string()),
                        _ => None,
                    };
                    if let (Prepared::Ident(n), true) = (&prepared, name.is_some()) {
                        let n = n.to_string();
                        self.assign_ident(&n, |c| c.expr_named(value, &n));
                        return;
                    }
                    match name {
                        Some(n) => self.expr_named(value, &n),
                        None => self.expr(value),
                    }
                    if matches!(prepared, Prepared::Invalid) {
                        self.emit(Op::Pop);
                        self.throw_reference_error("invalid assignment target");
                        return;
                    }
                    self.store_keep(prepared);
                }
                _ => {
                    self.expr(value);
                    self.emit(Op::Dup);
                    self.pattern(target, BindMode::Assign);
                }
            }
            return;
        }
        let logical = matches!(op, AssignOp::And | AssignOp::Or | AssignOp::Coalesce);
        let target_expr;
        let arg: &Expr = match &target.kind {
            PatternKind::Expr(e) => e,
            PatternKind::Ident(name) => {
                target_expr = Expr {
                    span: target.span,
                    kind: ExprKind::Ident(name.clone()),
                };
                &target_expr
            }
            _ => {
                self.expr(value);
                self.throw_reference_error("invalid assignment target");
                return;
            }
        };
        let mut inner = arg;
        while let ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        let ident = match &inner.kind {
            ExprKind::Ident(n) => Some(n.clone()),
            _ => None,
        };
        let prepared = self.prepare_expr_target(inner);
        match &prepared {
            Prepared::Ident(name) => self.load_name(name),
            Prepared::DynamicIdent(name) => {
                let a = self.atom(name);
                self.emit(Op::GetRef(a));
            }
            Prepared::Named(a) => {
                self.emit(Op::GetNamedKeep(*a));
            }
            Prepared::Computed => {
                self.emit(Op::GetComputedKeep);
            }
            Prepared::Private => {
                self.emit(Op::GetPrivateKeep);
            }
            Prepared::SuperNamed(a) => {
                self.emit(Op::Dup2);
                self.emit(Op::GetSuperNamed(*a));
            }
            Prepared::SuperComputed => self.super_computed_keep(),
            _ => {
                self.expr(value);
                self.store_prepared(Prepared::Invalid, BindMode::Assign);
                return;
            }
        }
        if logical {
            let (short, end) = (self.new_label(), self.new_label());
            let jump = match op {
                AssignOp::And => Op::JumpIfFalseKeep(0),
                AssignOp::Or => Op::JumpIfTrueKeep(0),
                _ => Op::JumpIfNotNullishKeep(0),
            };
            self.emit_jump(jump, short);
            match &ident {
                Some(n) => self.expr_named(value, n),
                None => self.expr(value),
            }
            let refs = match &prepared {
                Prepared::Ident(_) => 0,
                Prepared::DynamicIdent(_) | Prepared::Named(_) => 1,
                Prepared::SuperComputed => 3,
                _ => 2,
            };
            self.store_keep(prepared);
            self.jump(end);
            self.bind(short);
            // `[refs old] → [old]`
            for _ in 0..refs {
                self.emit(Op::Swap);
                self.emit(Op::Pop);
            }
            self.bind(end);
            return;
        }
        self.expr(value);
        if let Some(op) = assign_binary(op) {
            self.emit(op);
        }
        self.store_keep(prepared);
    }

    /// `[v] → [v]`: PutValue to a static binding.
    pub(crate) fn assign_local_value_pub(&mut self, name: &str) {
        let Binding::Local { hops, slot, flags } = self.resolve(name) else {
            let a = self.atom(name);
            // Not reached: callers check the binding is static.
            self.emit(Op::GetName(a));
            return;
        };
        if flags & crate::env::BIND_CONST != 0 {
            self.emit(Op::ThrowConstAssign { hops, slot });
        } else if flags & crate::env::BIND_SLOPPY_CONST != 0 {
            if self.strict() {
                self.emit(Op::ThrowConstAssign { hops, slot });
            }
        } else {
            self.emit(Op::SetLocal { hops, slot });
        }
    }

    // ----- generators -----

    fn is_async_generator(&self) -> bool {
        let f = &self.fr().info.flags;
        f.is_async && f.is_generator
    }

    /// `yield v`.
    fn yield_expr(&mut self, argument: Option<&Expr>) {
        match argument {
            Some(x) => self.expr(x),
            None => {
                self.emit(Op::Undefined);
            }
        }
        let is_async = self.is_async_generator();
        if is_async {
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
            self.emit(Op::AsyncGenYield);
        } else {
            self.emit(Op::Yield);
        }
        let (ret, after) = (self.new_label(), self.new_label());
        self.emit_jump(Op::ResumeKind { ret: 0 }, ret);
        self.jump(after);
        self.bind(ret);
        if is_async {
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
        }
        self.emit_return();
        self.bind(after);
    }

    /// `yield* v`.
    fn yield_star(&mut self, argument: Option<&Expr>) {
        let is_async = self.is_async_generator();
        match argument {
            Some(x) => self.expr(x),
            None => {
                self.emit(Op::Undefined);
            }
        }
        // Registers: iterator, next method, received value, received mode.
        let iter = self.alloc_regs(4);
        self.emit(Op::GetIterator { iter, is_async });
        self.emit(Op::Undefined);
        self.emit(Op::StoreReg(iter + 2));
        self.emit(Op::Int(0));
        self.emit(Op::StoreReg(iter + 3));
        let (top, no_throw, ret_direct, ret_done, done) = (
            self.new_label(),
            self.new_label(),
            self.new_label(),
            self.new_label(),
            self.new_label(),
        );
        self.bind(top);
        self.emit_jump2(
            Op::YieldStarCall {
                iter,
                no_throw: 0,
                ret: 0,
            },
            no_throw,
            ret_direct,
        );
        if is_async {
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
        }
        self.emit_jump2(
            Op::YieldStarResult {
                iter,
                done: 0,
                ret: 0,
                is_async,
            },
            done,
            ret_done,
        );
        if is_async {
            self.emit(Op::AsyncGenYield);
        } else {
            self.emit(Op::YieldRaw);
        }
        // Resumed with `[value mode]`.
        self.emit(Op::StoreReg(iter + 3));
        self.emit(Op::StoreReg(iter + 2));
        if is_async {
            // AsyncGeneratorUnwrapYieldResumption awaits a return value.
            self.emit(Op::LoadReg(iter + 3));
            self.emit(Op::Int(2));
            self.emit(Op::StrictEq);
            self.emit_jump(Op::JumpIfFalse(0), top);
            self.emit(Op::LoadReg(iter + 2));
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
            self.emit(Op::StoreReg(iter + 2));
        }
        self.jump(top);
        // The inner iterator has no `throw`: close it and throw.
        self.bind(no_throw);
        if is_async {
            let skip = self.new_label();
            self.emit_jump(Op::AsyncIteratorCloseCall { iter, skip: 0 }, skip);
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
            let c = self.str_const("iterator return() result is not an object");
            self.emit(Op::CheckObject(c));
            self.emit(Op::Pop);
            self.bind(skip);
        } else {
            self.emit(Op::IteratorClose { iter });
        }
        self.throw_type_error("the iterator does not have a 'throw' method");
        // Return received and the inner iterator has no `return`.
        self.bind(ret_direct);
        if is_async {
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
        }
        self.emit_return();
        // `return()` of the inner iterator reported done.
        self.bind(ret_done);
        if is_async {
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
        }
        self.emit_return();
        self.bind(done);
    }
}
