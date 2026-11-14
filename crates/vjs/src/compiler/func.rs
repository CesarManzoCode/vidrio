//! Functions (FunctionDeclarationInstantiation compiled into the prologue
//! of each body) and classes (ClassDefinitionEvaluation).

use std::rc::Rc;

use crate::ast::{
    Class, ClassElementKind, Expr, Function, FunctionBody, FunctionKind, Params, PropertyKey,
    ScopeInfo, Span, Stmt,
};
use crate::bytecode::{ClassName, CodeKind, FunctionFlags, Hidden, Op};
use crate::compiler::expr::{is_anonymous_function, method_op, static_key_name};
use crate::compiler::scan::{
    LexKind, class_private_names, expected_argument_count, functions_to_initialize,
    has_parameter_expressions, hoisted_functions, param_names, top_lexical_declarations,
    using_kind,
};
use crate::compiler::stmt::BindMode;
use crate::compiler::{
    Compiler, FnInfo, FnState, SLOPPY_CONST, Scope, ScopeKind, hidden_name, lexical_flags,
};
use crate::env::{BIND_CONST, BIND_TDZ, ScopeNames};

/// The name a function object gets when it is created.
pub(crate) enum FnName {
    /// Its own name (or `""`).
    Own,
    /// NamedEvaluation with a static name.
    Static(String),
    /// Set at run time from a property key.
    Keyed,
}

/// The parts of a body that FunctionDeclarationInstantiation looks at.
struct BodyParts<'a> {
    params: Option<&'a Params>,
    body: &'a [Stmt],
    scope: &'a ScopeInfo,
}

impl Compiler<'_> {
    /// Compiles a function and adds it to the current code block; returns
    /// its index in `functions`.
    pub(crate) fn function(&mut self, f: &Function, name: FnName, named_expr: bool) -> u32 {
        let flags = FunctionFlags {
            is_arrow: f.kind == FunctionKind::Arrow,
            is_method: f.kind == FunctionKind::Method,
            is_async: f.is_async,
            is_generator: f.is_generator,
            is_class_constructor: false,
            is_derived: false,
            is_constructor: f.kind == FunctionKind::Normal && !f.is_async && !f.is_generator,
        };
        let info = FnInfo {
            flags,
            method: f.kind == FunctionKind::Method,
            ..FnInfo::default()
        };
        self.compile_function(f, info, name, named_expr, f.span)
    }

    /// A method of an object literal or class: named at run time.
    pub(crate) fn method_function(&mut self, f: &Function) -> u32 {
        let info = FnInfo {
            flags: FunctionFlags {
                is_method: true,
                is_async: f.is_async,
                is_generator: f.is_generator,
                ..FunctionFlags::default()
            },
            method: true,
            ..FnInfo::default()
        };
        self.compile_function(f, info, FnName::Keyed, false, f.span)
    }

    fn add_function(&mut self, code: crate::bytecode::Code) -> u32 {
        let fs = self.fs();
        fs.functions.push(Rc::new(code));
        (fs.functions.len() - 1) as u32
    }

    fn compile_function(
        &mut self,
        f: &Function,
        info: FnInfo,
        name: FnName,
        named_expr: bool,
        span: Span,
    ) -> u32 {
        let derived = info.flags.is_derived;
        let base_class = info.flags.is_class_constructor && !derived;
        let mut fs = FnState::new(CodeKind::Function, info, f.strict);
        fs.this_in_slot = derived;
        self.fns.push(fs);
        let own_name = f.name.as_ref().map(|n| n.name.clone());
        if named_expr {
            if let Some(n) = &own_name {
                let mut s = Scope::new(ScopeKind::FuncName);
                s.declare(n, SLOPPY_CONST);
                self.push_scope(s);
            }
        }
        if base_class {
            self.emit(Op::InitThisFields);
        }
        let body: &[Stmt] = match &f.body {
            FunctionBody::Block(stmts) => stmts,
            FunctionBody::Expr(_) => &[],
        };
        self.prologue(&BodyParts {
            params: Some(&f.params),
            body,
            scope: &f.scope,
        });
        match &f.body {
            FunctionBody::Block(stmts) => self.function_statements(stmts),
            FunctionBody::Expr(e) => {
                self.expr(e);
                self.emit_return();
            }
        }
        self.emit(Op::Undefined);
        self.emit(Op::Return);
        while !self.fr().scopes.is_empty() {
            self.pop_scope(false);
        }
        let name_atom = match name {
            FnName::Own => Some(
                self.agent
                    .heap
                    .intern_str(own_name.as_deref().unwrap_or("")),
            ),
            FnName::Static(s) => Some(self.agent.heap.intern_str(&s)),
            FnName::Keyed => None,
        };
        let length = expected_argument_count(&f.params);
        let mut code = self.finish_fn(name_atom, length, Some(span));
        if named_expr {
            if let Some(n) = &own_name {
                let a = self.agent.heap.intern_str(n);
                code.name_scope = Some(Rc::new(ScopeNames {
                    names: vec![a],
                    flags: vec![SLOPPY_CONST],
                    ..ScopeNames::default()
                }));
            }
        }
        self.add_function(code)
    }

    /// The statements of a function body (with disposal of `using`
    /// declarations at its top level).
    fn function_statements(&mut self, stmts: &[Stmt]) {
        match using_kind(stmts) {
            Some(is_async) => self.using_block(is_async, |c| c.stmts(stmts)),
            None => self.stmts(stmts),
        }
    }

    /// FunctionDeclarationInstantiation as code.
    fn prologue(&mut self, parts: &BodyParts<'_>) {
        let empty = Params {
            is_simple: true,
            ..Params::default()
        };
        let params = parts.params.unwrap_or(&empty);
        let info = self.fr().info.clone();
        let strict = self.strict();
        let arrow = info.flags.is_arrow;
        let names = param_names(params);
        let has_param_exprs = has_parameter_expressions(params);
        let lexical = top_lexical_declarations(parts.body);
        let hoisted = hoisted_functions(parts.body);
        let functions = functions_to_initialize(&hoisted);
        let function_names: Vec<String> = functions
            .iter()
            .filter_map(|f| f.name.as_ref().map(|n| n.name.clone()))
            .collect();
        let has_eval = parts.scope.has_direct_eval;
        let arguments_needed = !arrow
            && !info.field_init
            && !info.static_block
            && (parts.scope.uses_arguments || has_eval)
            && !names.iter().any(|n| n == "arguments")
            && (has_param_exprs
                || !(function_names.iter().any(|n| n == "arguments")
                    || lexical.iter().any(|(n, _)| n == "arguments")));
        let separate_lexical = !strict && has_eval;

        let mut scope = Scope::new(ScopeKind::Function);
        scope.var_scope = !has_param_exprs;
        scope.tainted = !strict && has_eval && !has_param_exprs;
        for n in &names {
            scope.declare(n, if has_param_exprs { BIND_TDZ } else { 0 });
        }
        if arguments_needed {
            scope.declare("arguments", 0);
        }
        if !has_param_exprs {
            for v in &parts.scope.var_names {
                scope.declare(v, 0);
            }
            if !separate_lexical {
                for (n, k) in &lexical {
                    scope.declare(n, lex_kind_flags(*k));
                }
            }
        }
        if !arrow && (has_eval || info.flags.is_derived) {
            for h in [Hidden::This, Hidden::NewTarget, Hidden::Function] {
                let flags = if h == Hidden::This && info.flags.is_derived {
                    BIND_TDZ
                } else {
                    0
                };
                let slot = scope.declare(hidden_name(h), flags);
                scope.hidden[h as usize] = Some(slot);
            }
        }
        let param_slots: Vec<u32> = names
            .iter()
            .map(|n| scope.find(n).expect("parameter slot") as u32)
            .collect();
        let arguments_slot = scope.find("arguments").map(|s| s as u32);
        self.push_scope(scope);
        if !arrow {
            let idx = self.fr().scopes.len() - 1;
            self.fs().fn_scope = Some(idx);
            self.emit(Op::InitHidden);
        }

        if arguments_needed {
            let mapped = !strict && params.is_simple;
            if mapped {
                let mut map = vec![None; params.items.len()];
                let mut seen: Vec<&str> = Vec::new();
                for i in (0..params.items.len()).rev() {
                    let n = names[i].as_str();
                    if !seen.contains(&n) {
                        seen.push(n);
                        map[i] = Some(param_slots[i]);
                    }
                }
                self.fs().arg_map = map;
            }
            self.emit(Op::CreateArguments { mapped });
            let slot = arguments_slot.expect("arguments slot");
            self.emit(Op::InitLocal { hops: 0, slot });
        }

        if !has_param_exprs {
            for (i, slot) in param_slots.iter().enumerate() {
                self.emit(Op::GetArg(i as u32));
                self.emit(Op::InitLocal {
                    hops: 0,
                    slot: *slot,
                });
            }
        } else {
            for (i, p) in params.items.iter().enumerate() {
                self.emit(Op::GetArg(i as u32));
                if let Some(default) = &p.default {
                    let skip = self.new_label();
                    self.emit_jump(Op::JumpIfNotUndefined(0), skip);
                    self.emit(Op::Pop);
                    match &p.target.kind {
                        crate::ast::PatternKind::Ident(n) => {
                            let n = n.clone();
                            self.expr_named(default, &n);
                        }
                        _ => self.expr(default),
                    }
                    self.bind(skip);
                }
                self.pattern(&p.target, BindMode::Init);
            }
            if let Some(rest) = &params.rest {
                self.emit(Op::RestArgs(params.items.len() as u32));
                self.pattern(rest, BindMode::Init);
            }
        }

        if has_param_exprs {
            let mut vs = Scope::new(ScopeKind::Var);
            vs.var_scope = true;
            vs.tainted = !strict && has_eval;
            for v in &parts.scope.var_names {
                vs.declare(v, 0);
            }
            if !separate_lexical {
                for (n, k) in &lexical {
                    vs.declare(n, lex_kind_flags(*k));
                }
            }
            let copies: Vec<(u32, u32)> = parts
                .scope
                .var_names
                .iter()
                .filter_map(|v| {
                    let from = names
                        .iter()
                        .position(|n| n == v)
                        .map(|i| param_slots[i])
                        .or(if v == "arguments" {
                            arguments_slot
                        } else {
                            None
                        })?;
                    Some((from, vs.find(v)? as u32))
                })
                .collect();
            self.push_scope(vs);
            for (from, to) in copies {
                self.emit(Op::GetLocal {
                    hops: 1,
                    slot: from,
                });
                self.emit(Op::InitLocal { hops: 0, slot: to });
            }
        }
        if separate_lexical && !lexical.is_empty() {
            let mut ls = Scope::new(ScopeKind::Lexical);
            for (n, k) in &lexical {
                ls.declare(n, lex_kind_flags(*k));
            }
            self.push_scope(ls);
        }
        for f in functions {
            let idx = self.function(f, FnName::Own, false);
            self.emit(Op::Closure(idx));
            let name = f.name.as_ref().map(|n| n.name.clone()).unwrap_or_default();
            self.init_name(&name);
        }
        if info.flags.is_generator {
            self.emit(Op::InitialYield);
        }
    }

    // ----- classes -----

    /// A class definition; `name` is the NamedEvaluation name of an
    /// anonymous class.
    pub(crate) fn class(&mut self, c: &Class, name: Option<&str>) {
        let cname = match (&c.name, name) {
            (Some(n), _) => ClassName::Atom(self.atom(&n.name)),
            (None, Some(s)) => ClassName::Atom(self.atom(s)),
            (None, None) => ClassName::Empty,
        };
        self.class_with(c, cname);
    }

    /// `[] → [F]`: ClassDefinitionEvaluation.
    pub(crate) fn class_with(&mut self, c: &Class, cname: ClassName) {
        let mut scope = Scope::new(ScopeKind::Class);
        if let Some(n) = &c.name {
            scope.declare(&n.name, BIND_TDZ | BIND_CONST);
        }
        let privates = class_private_names(&c.elements);
        for p in &privates {
            scope.declare(p, BIND_CONST);
        }
        self.push_scope(scope);
        for p in &privates {
            self.emit_str(p);
            self.emit(Op::NewPrivateName);
            self.init_name(p);
        }
        let derived = c.heritage.is_some();
        match &c.heritage {
            Some(h) => {
                self.expr(h);
                self.emit(Op::ClassHeritage);
            }
            None => {
                self.emit(Op::ClassNoHeritage);
            }
        }
        let ctor = match &c.constructor {
            Some(f) => {
                let info = FnInfo {
                    flags: FunctionFlags {
                        is_method: true,
                        is_class_constructor: true,
                        is_derived: derived,
                        ..FunctionFlags::default()
                    },
                    method: true,
                    ..FnInfo::default()
                };
                self.compile_function(f, info, FnName::Keyed, false, c.span)
            }
            None => self.default_constructor(derived, c.span),
        };
        self.emit(Op::CreateClass {
            func: ctor,
            name: cname,
            derived,
        });

        enum Static {
            Field { reg: u32, func: u32, private: bool },
            Block(u32),
        }
        let mut statics = Vec::new();
        for el in &c.elements {
            match &el.kind {
                ClassElementKind::Method {
                    key,
                    kind,
                    function,
                } => {
                    let func = self.method_function(function);
                    if let PropertyKey::Private(n) = key {
                        self.load_private_name(n);
                        self.emit(Op::DefinePrivateMethod {
                            func,
                            kind: method_op(*kind),
                            is_static: el.is_static,
                        });
                    } else {
                        self.property_key(key);
                        self.emit(Op::DefineClassMethod {
                            func,
                            kind: method_op(*kind),
                            is_static: el.is_static,
                        });
                    }
                }
                ClassElementKind::Field { key, value, scope } => {
                    let private = matches!(key, PropertyKey::Private(_));
                    self.property_key(key);
                    let func = match value {
                        Some(v) => self.field_initializer(v, static_key_name(key), scope, el.span),
                        None => u32::MAX,
                    };
                    if el.is_static {
                        let reg = self.alloc_reg();
                        self.emit(Op::StoreReg(reg));
                        statics.push(Static::Field { reg, func, private });
                    } else {
                        self.emit(Op::AddField { func, private });
                    }
                }
                ClassElementKind::StaticBlock { body, scope } => {
                    let func = self.static_block(body, scope, el.span);
                    statics.push(Static::Block(func));
                }
            }
        }
        if let Some(n) = &c.name {
            self.emit(Op::Dup2);
            self.emit(Op::Pop);
            let n = n.name.clone();
            self.init_name(&n);
        }
        self.emit(Op::Pop);
        for s in statics {
            match s {
                Static::Field { reg, func, private } => {
                    self.emit(Op::LoadReg(reg));
                    self.emit(Op::DefineStaticField { func, private });
                }
                Static::Block(func) => {
                    self.emit(Op::StaticBlock(func));
                }
            }
        }
        self.pop_scope(true);
    }

    /// Starts the code of a method-like body (field initializer, static
    /// block, default constructor) with an empty parameter list.
    fn begin_method_body(&mut self, info: FnInfo) {
        let derived = info.flags.is_derived;
        let mut fs = FnState::new(CodeKind::Function, info, true);
        fs.this_in_slot = derived;
        self.fns.push(fs);
    }

    fn end_method_body(&mut self, span: Span) -> u32 {
        self.emit(Op::Undefined);
        self.emit(Op::Return);
        while !self.fr().scopes.is_empty() {
            self.pop_scope(false);
        }
        let empty = self.agent.heap.intern_str("");
        let code = self.finish_fn(Some(empty), 0, Some(span));
        self.add_function(code)
    }

    /// A class field initializer: a method returning the initial value. It
    /// receives the field key as its argument (NamedEvaluation of computed
    /// keys).
    fn field_initializer(
        &mut self,
        value: &Expr,
        name: Option<String>,
        scope: &ScopeInfo,
        span: Span,
    ) -> u32 {
        let info = FnInfo {
            flags: FunctionFlags {
                is_method: true,
                ..FunctionFlags::default()
            },
            field_init: true,
            method: true,
            ..FnInfo::default()
        };
        self.begin_method_body(info);
        self.prologue(&BodyParts {
            params: None,
            body: &[],
            scope,
        });
        if is_anonymous_function(value) {
            match name {
                Some(n) => self.expr_named(value, &n),
                None => {
                    self.emit(Op::GetArg(0));
                    self.expr_keyed(value);
                    self.emit(Op::Swap);
                    self.emit(Op::Pop);
                }
            }
        } else {
            self.expr(value);
        }
        self.emit(Op::Return);
        self.end_method_body(span)
    }

    /// A `static { … }` block: a method called with the class as `this`.
    fn static_block(&mut self, body: &[Stmt], scope: &ScopeInfo, span: Span) -> u32 {
        let info = FnInfo {
            flags: FunctionFlags {
                is_method: true,
                ..FunctionFlags::default()
            },
            static_block: true,
            method: true,
            ..FnInfo::default()
        };
        self.begin_method_body(info);
        self.prologue(&BodyParts {
            params: None,
            body,
            scope,
        });
        self.function_statements(body);
        self.end_method_body(span)
    }

    /// The default constructor of a class without one.
    fn default_constructor(&mut self, derived: bool, span: Span) -> u32 {
        let info = FnInfo {
            flags: FunctionFlags {
                is_method: true,
                is_class_constructor: true,
                is_derived: derived,
                ..FunctionFlags::default()
            },
            method: true,
            ..FnInfo::default()
        };
        self.begin_method_body(info);
        if !derived {
            self.emit(Op::InitThisFields);
        }
        let scope = ScopeInfo::default();
        self.prologue(&BodyParts {
            params: None,
            body: &[],
            scope: &scope,
        });
        if derived {
            self.emit(Op::LoadFunction);
            self.emit(Op::GetSuperConstructor);
            self.emit(Op::LoadNewTarget);
            self.emit(Op::SuperCallForward);
            match self.resolve_hidden(Hidden::This) {
                Some((hops, slot)) => self.emit(Op::BindThis { hops, slot }),
                None => self.emit(Op::BindThisDyn),
            };
            self.emit(Op::LoadFunction);
            self.emit(Op::InitInstanceElements);
            self.emit(Op::Pop);
        }
        self.emit(Op::Undefined);
        self.emit(Op::Return);
        while !self.fr().scopes.is_empty() {
            self.pop_scope(false);
        }
        let code = self.finish_fn(None, 0, Some(span));
        self.add_function(code)
    }
}

fn lex_kind_flags(kind: LexKind) -> u8 {
    match kind {
        LexKind::Const => lexical_flags(true),
        LexKind::Function => 0,
        _ => lexical_flags(false),
    }
}
