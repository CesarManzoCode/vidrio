//! Module code (ECMA-262 §16.2.1.6.4 InitializeEnvironment and
//! §16.2.1.6.5 ExecuteModule).
//!
//! A module compiles to two code blocks that share the layout of the
//! module environment: `init` creates the hoisted function declarations
//! (InitializeEnvironment runs it at link time, so functions exported
//! through an import cycle exist before any module body runs) and `body`
//! evaluates the module items. Both run with the module environment as the
//! frame environment, so neither pushes a scope.
//!
//! Import bindings are declared with [`BIND_IMPORT`]: the compiler resolves
//! them by name, and the run-time lookup follows the indirection recorded
//! in the environment (CreateImportBinding).

use std::rc::Rc;

use crate::agent::Agent;
use crate::ast::{ExportDecl, Function, ImportSpecifier, Program, Stmt, StmtKind};
use crate::bytecode::{Code, CodeKind, Op};
use crate::compiler::func::FnName;
use crate::compiler::scan::{LexKind, top_lexical_declarations, using_kind};
use crate::compiler::{Compiler, FnInfo, FnState, Scope, ScopeKind, hidden_name, lexical_flags};
use crate::env::{BIND_CONST, BIND_IMPORT, BIND_TDZ, ScopeNames};

/// The local name of a default export that has no name of its own.
pub const DEFAULT_LOCAL: &str = "*default*";

/// The compiled code of a module.
pub struct ModuleCode {
    /// Creates the hoisted functions in the module environment.
    pub init: Rc<Code>,
    /// Evaluates the module items.
    pub body: Rc<Code>,
    /// Layout of the module environment.
    pub layout: Rc<ScopeNames>,
    /// `[[HasTLA]]`: the body contains an `await` outside functions.
    pub has_tla: bool,
}

/// The function declarations of a module body with the name they bind.
fn module_functions(body: &[Stmt]) -> Vec<(&Function, String)> {
    let mut out = Vec::new();
    for s in body {
        match &s.kind {
            StmtKind::Function(f) => {
                if let Some(n) = &f.name {
                    out.push((&**f, n.name.clone()));
                }
            }
            StmtKind::Export(e) => match &**e {
                ExportDecl::Declaration(Stmt {
                    kind: StmtKind::Function(f),
                    ..
                }) => {
                    if let Some(n) = &f.name {
                        out.push((&**f, n.name.clone()));
                    }
                }
                ExportDecl::DefaultFunction(f) => {
                    let name = f
                        .name
                        .as_ref()
                        .map_or(DEFAULT_LOCAL.to_string(), |n| n.name.clone());
                    out.push((&**f, name));
                }
                _ => {}
            },
            _ => {}
        }
    }
    out
}

/// The module environment: hidden `this`, imports, vars, functions and
/// lexical declarations.
fn module_scope(program: &Program) -> Scope {
    let mut scope = Scope::new(ScopeKind::Var);
    scope.var_scope = true;
    let this_slot = scope.declare(hidden_name(crate::bytecode::Hidden::This), 0);
    scope.hidden[0] = Some(this_slot);
    for s in &program.body {
        if let StmtKind::Import(decl) = &s.kind {
            for spec in &decl.specifiers {
                match spec {
                    ImportSpecifier::Default(n) | ImportSpecifier::Named { local: n, .. } => {
                        scope.declare(&n.name, BIND_IMPORT);
                    }
                    ImportSpecifier::Namespace(n) => {
                        scope.declare(&n.name, BIND_CONST | BIND_TDZ);
                    }
                }
            }
        }
    }
    for v in &program.scope.var_names {
        scope.declare(v, 0);
    }
    for (_, name) in module_functions(&program.body) {
        scope.declare(&name, 0);
    }
    let mut items: Vec<Stmt> = Vec::new();
    for s in &program.body {
        match &s.kind {
            StmtKind::Export(e) => match &**e {
                ExportDecl::Declaration(d) => items.push(d.clone()),
                ExportDecl::DefaultClass(c) => {
                    let name = c
                        .name
                        .as_ref()
                        .map_or(DEFAULT_LOCAL.to_string(), |n| n.name.clone());
                    scope.declare(&name, lexical_flags(false));
                }
                ExportDecl::DefaultExpr(_) => {
                    scope.declare(DEFAULT_LOCAL, lexical_flags(false));
                }
                _ => {}
            },
            _ => items.push(s.clone()),
        }
    }
    for (name, kind) in top_lexical_declarations(&items) {
        scope.declare(&name, lexical_flags(kind == LexKind::Const));
    }
    scope
}

impl Compiler<'_> {
    /// The module items of the body (imports and re-exports compile to
    /// nothing; hoisted functions were created by the init code).
    fn module_items(&mut self, body: &[Stmt]) {
        for s in body {
            match &s.kind {
                StmtKind::Import(_) => {}
                StmtKind::Export(e) => match &**e {
                    ExportDecl::Named { .. }
                    | ExportDecl::All { .. }
                    | ExportDecl::DefaultFunction(_) => {}
                    ExportDecl::Declaration(d) => self.stmt(d),
                    ExportDecl::DefaultClass(c) => {
                        self.class(c, Some("default"));
                        match &c.name {
                            Some(n) => self.init_name(&n.name.clone()),
                            None => self.init_name(DEFAULT_LOCAL),
                        }
                    }
                    ExportDecl::DefaultExpr(e) => {
                        self.expr_named(e, "default");
                        self.init_name(DEFAULT_LOCAL);
                    }
                },
                _ => self.stmt(s),
            }
        }
    }
}

/// Compiles a module (`program` must have the Module goal).
pub fn compile_module(agent: &mut Agent, program: &Program, source: Rc<str>) -> ModuleCode {
    let scope = module_scope(program);
    let mut c = Compiler::new(agent, source);
    let layout = Rc::new(c.scope_names(&scope));

    // InitializeEnvironment: the hoisted functions.
    let mut fs = FnState::new(CodeKind::Module, FnInfo::default(), true);
    fs.scopes.push(scope.clone());
    c.fns.push(fs);
    for (f, name) in module_functions(&program.body) {
        let fn_name = if f.name.is_some() {
            FnName::Own
        } else {
            FnName::Static("default".to_string())
        };
        let idx = c.function(f, fn_name, false);
        c.emit(Op::Closure(idx));
        c.init_name(&name);
    }
    c.emit(Op::Undefined);
    c.emit(Op::Return);
    let init = Rc::new(c.finish_fn(None, 0, None));

    // ExecuteModule: the module items.
    let mut fs = FnState::new(CodeKind::Module, FnInfo::default(), true);
    fs.scopes.push(scope);
    c.fns.push(fs);
    match using_kind(&program.body) {
        Some(is_async) => c.using_block(is_async, |c| c.module_items(&program.body)),
        None => c.module_items(&program.body),
    }
    c.emit(Op::Undefined);
    c.emit(Op::Return);
    let body = c.finish_fn(None, 0, None);
    let has_tla = body.ops.iter().any(|op| matches!(op, Op::Await));
    ModuleCode {
        init,
        body: Rc::new(body),
        layout,
        has_tla,
    }
}
