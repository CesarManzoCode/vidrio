//! Statements, declarations and the prologues of script and eval code.

use crate::ast::{
    CatchClause, Expr, ForHead, ForInit, Pattern, PatternKind, Program, Stmt, StmtKind, SwitchCase,
    VarDecl, VarKind,
};
use crate::bytecode::{CodeKind, Declarations, Op};
use crate::compiler::scan::{
    LexKind, functions_to_initialize, hoisted_functions, lexical_declarations,
    top_lexical_declarations, using_kind,
};
use crate::compiler::{
    BreakKind, Compiler, Control, FinallyInfo, Scope, ScopeKind, bound_names, lexical_flags,
};

/// How a pattern stores its values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindMode {
    /// InitializeReferencedBinding (`let`, `const`, parameters, `catch`).
    Init,
    /// PutValue (`var`, assignment).
    Assign,
}

impl Compiler<'_> {
    pub(crate) fn stmts(&mut self, list: &[Stmt]) {
        for s in list {
            self.stmt(s);
        }
    }

    fn is_loop(s: &Stmt) -> bool {
        matches!(
            s.kind,
            StmtKind::For { .. }
                | StmtKind::ForIn { .. }
                | StmtKind::ForOf { .. }
                | StmtKind::While { .. }
                | StmtKind::DoWhile { .. }
        )
    }

    pub(crate) fn stmt(&mut self, s: &Stmt) {
        if let StmtKind::Labeled { label, body } = &s.kind {
            let mut labels = std::mem::take(&mut self.fs().pending_labels);
            labels.push(label.name.clone());
            if Self::is_loop(body) {
                self.fs().pending_labels = labels;
                self.stmt(body);
            } else {
                let brk = self.new_label();
                self.fs().control.push(Control::Breakable {
                    labels,
                    kind: BreakKind::Label,
                    brk,
                    cont: None,
                    iter: None,
                });
                self.stmt(body);
                self.fs().control.pop();
                self.bind(brk);
            }
            return;
        }
        let labels = std::mem::take(&mut self.fs().pending_labels);
        let completion = self.fr().completion;
        match &s.kind {
            StmtKind::Empty | StmtKind::Function(_) => {}
            StmtKind::Debugger => {
                self.emit(Op::Debugger);
            }
            StmtKind::Expr(e) => {
                self.expr(e);
                self.emit(if completion {
                    Op::SetCompletion
                } else {
                    Op::Pop
                });
            }
            StmtKind::Directive { value, .. } => {
                if completion {
                    let c = self.string_const(value);
                    self.emit(Op::Const(c));
                    self.emit(Op::SetCompletion);
                }
            }
            StmtKind::Block(list) => self.block(list),
            StmtKind::Var(decl) => self.var_decl(decl),
            StmtKind::Class(c) => {
                let name = c.name.as_ref().map(|n| n.name.clone());
                self.class(c, name.as_deref());
                self.init_name(name.as_deref().unwrap_or("default"));
            }
            StmtKind::If {
                test,
                consequent,
                alternate,
            } => {
                if completion {
                    self.emit(Op::ClearCompletion);
                }
                self.expr(test);
                let else_label = self.new_label();
                self.emit_jump(Op::JumpIfFalse(0), else_label);
                self.stmt(consequent);
                match alternate {
                    Some(alt) => {
                        let end = self.new_label();
                        self.jump(end);
                        self.bind(else_label);
                        self.stmt(alt);
                        self.bind(end);
                    }
                    None => self.bind(else_label),
                }
            }
            StmtKind::While { test, body } => {
                if completion {
                    self.emit(Op::ClearCompletion);
                }
                let (brk, cont) = (self.new_label(), self.new_label());
                self.bind(cont);
                self.expr(test);
                self.emit_jump(Op::JumpIfFalse(0), brk);
                self.loop_body(labels, brk, cont, None, body);
                self.jump(cont);
                self.bind(brk);
            }
            StmtKind::DoWhile { body, test } => {
                if completion {
                    self.emit(Op::ClearCompletion);
                }
                let (brk, cont, top) = (self.new_label(), self.new_label(), self.new_label());
                self.bind(top);
                self.loop_body(labels, brk, cont, None, body);
                self.bind(cont);
                self.expr(test);
                self.emit_jump(Op::JumpIfTrue(0), top);
                self.bind(brk);
            }
            StmtKind::For {
                init,
                test,
                update,
                body,
            } => self.for_stmt(
                labels,
                init.as_ref(),
                test.as_deref(),
                update.as_deref(),
                body,
            ),
            StmtKind::ForIn { left, right, body } => self.for_in(labels, left, right, body),
            StmtKind::ForOf {
                left,
                right,
                body,
                is_await,
            } => self.for_of(labels, left, right, body, *is_await),
            StmtKind::Continue(label) => {
                self.emit_break(label.as_ref().map(|l| l.name.as_str()), true);
            }
            StmtKind::Break(label) => {
                self.emit_break(label.as_ref().map(|l| l.name.as_str()), false);
            }
            StmtKind::Return(arg) => {
                match arg {
                    Some(e) => self.expr(e),
                    None => {
                        self.emit(Op::Undefined);
                    }
                }
                if self.fr().info.flags.is_async && self.fr().info.flags.is_generator {
                    self.emit(Op::Await);
                    self.emit(Op::AwaitResume);
                }
                self.emit_return();
            }
            StmtKind::With { object, body } => {
                if completion {
                    self.emit(Op::ClearCompletion);
                }
                self.expr(object);
                self.emit(Op::PushWith);
                self.push_scope(Scope::new(ScopeKind::With));
                self.stmt(body);
                self.pop_scope(true);
            }
            StmtKind::Switch {
                discriminant,
                cases,
            } => self.switch(labels, discriminant, cases),
            StmtKind::Throw(e) => {
                self.expr(e);
                self.emit(Op::Throw);
            }
            StmtKind::Try {
                block,
                handler,
                finalizer,
            } => self.try_stmt(block, handler.as_ref(), finalizer.as_deref()),
            StmtKind::Labeled { .. } => unreachable!("handled above"),
            StmtKind::Import(_) | StmtKind::Export(_) => {
                // Module code is compiled by the module loader.
            }
        }
    }

    /// The body of a loop with its breakable entry.
    fn loop_body(
        &mut self,
        labels: Vec<String>,
        brk: crate::compiler::Label,
        cont: crate::compiler::Label,
        iter: Option<(u32, bool)>,
        body: &Stmt,
    ) {
        self.fs().control.push(Control::Breakable {
            labels,
            kind: BreakKind::Loop,
            brk,
            cont: Some(cont),
            iter,
        });
        self.stmt(body);
        self.fs().control.pop();
    }

    /// A block statement: its own scope if it declares anything lexical.
    pub(crate) fn block(&mut self, list: &[Stmt]) {
        let decls = lexical_declarations(list);
        if decls.is_empty() {
            self.stmts(list);
            return;
        }
        let mut scope = Scope::new(ScopeKind::Block);
        for (name, kind) in &decls {
            let flags = match kind {
                LexKind::Function => 0,
                LexKind::Const => lexical_flags(true),
                _ => lexical_flags(false),
            };
            scope.declare(name, flags);
        }
        self.push_scope(scope);
        self.instantiate_block_functions(list);
        match using_kind(list) {
            Some(is_async) => self.using_block(is_async, |c| c.stmts(list)),
            None => self.stmts(list),
        }
        self.pop_scope(true);
    }

    /// BlockDeclarationInstantiation of the functions of a block.
    pub(crate) fn instantiate_block_functions(&mut self, list: &[Stmt]) {
        for f in hoisted_functions(list) {
            let idx = self.function(f, super::func::FnName::Own, false);
            self.emit(Op::Closure(idx));
            let name = f.name.as_ref().map(|n| n.name.clone()).unwrap_or_default();
            self.init_name(&name);
        }
    }

    // ----- declarations -----

    pub(crate) fn var_decl(&mut self, decl: &VarDecl) {
        let is_using = matches!(decl.kind, VarKind::Using | VarKind::AwaitUsing);
        for d in &decl.declarations {
            match decl.kind {
                VarKind::Var => {
                    let Some(init) = &d.init else { continue };
                    if let PatternKind::Ident(name) = &d.target.kind {
                        self.assign_ident(name, |c| c.expr_named(init, name));
                        self.emit(Op::Pop);
                    } else {
                        self.expr(init);
                        self.pattern(&d.target, BindMode::Assign);
                    }
                }
                _ => {
                    match &d.init {
                        Some(init) => match &d.target.kind {
                            PatternKind::Ident(name) => self.expr_named(init, name),
                            _ => self.expr(init),
                        },
                        None => {
                            self.emit(Op::Undefined);
                        }
                    }
                    if is_using {
                        let reg = self.current_using_reg();
                        self.emit(Op::AddDisposable {
                            reg,
                            is_async: decl.kind == VarKind::AwaitUsing,
                        });
                    }
                    self.pattern(&d.target, BindMode::Init);
                }
            }
        }
    }

    /// Register of the disposable resource stack of the innermost `using`
    /// block.
    fn current_using_reg(&self) -> u32 {
        self.fr().using_regs.last().copied().unwrap_or(u32::MAX)
    }

    /// Wraps `body` in the disposal of the resources it adds: like a
    /// `try`/`finally` whose finally disposes them.
    pub(crate) fn using_block(&mut self, is_async: bool, body: impl FnOnce(&mut Self)) {
        let reg = self.alloc_reg();
        let kind_reg = self.alloc_reg();
        let value_reg = self.alloc_reg();
        let (entry, on_throw) = (self.new_label(), self.new_label());
        self.emit(Op::UsingInit(reg));
        self.emit_jump(Op::PushHandler(0), on_throw);
        self.fs().control.push(Control::Finally(FinallyInfo {
            kind_reg,
            value_reg,
            entry,
            exits: Vec::new(),
        }));
        self.fs().using_regs.push(reg);
        body(self);
        self.fs().using_regs.pop();
        let Some(Control::Finally(info)) = self.fs().control.pop() else {
            unreachable!("using control entry");
        };
        self.emit(Op::PopHandler);
        self.emit(Op::Int(0));
        self.emit(Op::StoreReg(kind_reg));
        self.jump(entry);
        self.bind(on_throw);
        self.emit(Op::StoreReg(value_reg));
        self.emit(Op::Int(1));
        self.emit(Op::StoreReg(kind_reg));
        self.bind(entry);
        let (step, on_error, done) = (self.new_label(), self.new_label(), self.new_label());
        self.bind(step);
        self.emit_jump(Op::PushHandler(0), on_error);
        self.emit_jump(Op::DisposeStep { reg, done: 0 }, done);
        if is_async {
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
        }
        self.emit(Op::Pop);
        self.emit(Op::PopHandler);
        self.jump(step);
        self.bind(on_error);
        self.emit(Op::DisposeError {
            kind: kind_reg,
            value: value_reg,
        });
        self.jump(step);
        self.bind(done);
        self.emit(Op::PopHandler);
        let table = self.new_exit_table();
        self.emit(Op::EndFinally {
            kind: kind_reg,
            value: value_reg,
            table,
        });
        let end = self.new_label();
        self.jump(end);
        self.emit_exit_trampolines(&info, table);
        self.bind(end);
    }

    // ----- loops -----

    fn for_stmt(
        &mut self,
        labels: Vec<String>,
        init: Option<&ForInit>,
        test: Option<&Expr>,
        update: Option<&Expr>,
        body: &Stmt,
    ) {
        if self.fr().completion {
            self.emit(Op::ClearCompletion);
        }
        let mut scoped = false;
        let mut per_iteration = false;
        match init {
            Some(ForInit::Var(decl)) if decl.kind != VarKind::Var => {
                let mut scope = Scope::new(ScopeKind::Block);
                for d in &decl.declarations {
                    let mut names = Vec::new();
                    bound_names(&d.target, &mut names);
                    for n in names {
                        scope.declare(&n, lexical_flags(decl.kind != VarKind::Let));
                    }
                }
                per_iteration = decl.kind == VarKind::Let;
                scoped = true;
                self.push_scope(scope);
                if matches!(decl.kind, VarKind::Using | VarKind::AwaitUsing) {
                    // `for (using x = …; …)` disposes once, after the loop.
                    let is_async = decl.kind == VarKind::AwaitUsing;
                    self.using_block(is_async, |c| {
                        c.var_decl(decl);
                        c.for_rest(labels, false, test, update, body);
                    });
                    self.pop_scope(true);
                    return;
                }
                self.var_decl(decl);
            }
            Some(ForInit::Var(decl)) => self.var_decl(decl),
            Some(ForInit::Expr(e)) => {
                self.expr(e);
                self.emit(Op::Pop);
            }
            None => {}
        }
        self.for_rest(labels, per_iteration, test, update, body);
        if scoped {
            self.pop_scope(true);
        }
    }

    fn for_rest(
        &mut self,
        labels: Vec<String>,
        per_iteration: bool,
        test: Option<&Expr>,
        update: Option<&Expr>,
        body: &Stmt,
    ) {
        let (brk, cont, top) = (self.new_label(), self.new_label(), self.new_label());
        if per_iteration {
            self.emit(Op::CopyScope);
        }
        self.bind(top);
        if let Some(t) = test {
            self.expr(t);
            self.emit_jump(Op::JumpIfFalse(0), brk);
        }
        self.loop_body(labels, brk, cont, None, body);
        self.bind(cont);
        if per_iteration {
            self.emit(Op::CopyScope);
        }
        if let Some(u) = update {
            self.expr(u);
            self.emit(Op::Pop);
        }
        self.jump(top);
        self.bind(brk);
    }

    /// Evaluates the right-hand side of `for-in`/`for-of`, inside a TDZ
    /// scope of the bound names for lexical heads.
    fn for_head_rhs(&mut self, left: &ForHead, right: &Expr) {
        let names = match left {
            ForHead::Var(decl) if decl.kind != VarKind::Var => {
                let mut names = Vec::new();
                for d in &decl.declarations {
                    bound_names(&d.target, &mut names);
                }
                names
            }
            _ => Vec::new(),
        };
        if names.is_empty() {
            self.expr(right);
            return;
        }
        let mut scope = Scope::new(ScopeKind::Block);
        for n in &names {
            scope.declare(n, lexical_flags(false));
        }
        self.push_scope(scope);
        self.expr(right);
        self.pop_scope(true);
    }

    /// Binds the value on the stack to the head of a `for-in`/`for-of`
    /// and runs the body; lexical heads get a fresh scope per iteration.
    fn for_bind_and_body(&mut self, left: &ForHead, body: &Stmt) {
        match left {
            ForHead::Var(decl) if decl.kind != VarKind::Var => {
                let d = &decl.declarations[0];
                let mut scope = Scope::new(ScopeKind::Block);
                let mut names = Vec::new();
                bound_names(&d.target, &mut names);
                for n in &names {
                    scope.declare(n, lexical_flags(decl.kind != VarKind::Let));
                }
                self.push_scope(scope);
                if matches!(decl.kind, VarKind::Using | VarKind::AwaitUsing) {
                    let is_async = decl.kind == VarKind::AwaitUsing;
                    let tmp = self.alloc_reg();
                    self.emit(Op::StoreReg(tmp));
                    self.using_block(is_async, |c| {
                        c.emit(Op::LoadReg(tmp));
                        let reg = c.current_using_reg();
                        c.emit(Op::AddDisposable { reg, is_async });
                        c.pattern(&d.target, BindMode::Init);
                        c.stmt(body);
                    });
                } else {
                    self.pattern(&d.target, BindMode::Init);
                    self.stmt(body);
                }
                self.pop_scope(true);
            }
            ForHead::Var(decl) => {
                let d = &decl.declarations[0];
                self.pattern(&d.target, BindMode::Assign);
                self.stmt(body);
            }
            ForHead::Target(p) => {
                self.pattern(p, BindMode::Assign);
                self.stmt(body);
            }
        }
    }

    fn for_in(&mut self, labels: Vec<String>, left: &ForHead, right: &Expr, body: &Stmt) {
        if self.fr().completion {
            self.emit(Op::ClearCompletion);
        }
        // Annex B: `for (var x = init in obj)`.
        if let ForHead::Var(decl) = left {
            if decl.kind == VarKind::Var {
                if let Some(d) = decl.declarations.first() {
                    if let (Some(init), PatternKind::Ident(name)) = (&d.init, &d.target.kind) {
                        self.assign_ident(name, |c| c.expr_named(init, name));
                        self.emit(Op::Pop);
                    }
                }
            }
        }
        self.for_head_rhs(left, right);
        let reg = self.alloc_reg();
        let (brk, cont) = (self.new_label(), self.new_label());
        self.emit_jump(Op::ForInPrepare { reg, skip: 0 }, brk);
        self.bind(cont);
        self.emit_jump(Op::ForInNext { reg, done: 0 }, brk);
        self.fs().control.push(Control::Breakable {
            labels,
            kind: BreakKind::Loop,
            brk,
            cont: Some(cont),
            iter: None,
        });
        self.for_bind_and_body(left, body);
        self.fs().control.pop();
        self.jump(cont);
        self.bind(brk);
    }

    fn for_of(
        &mut self,
        labels: Vec<String>,
        left: &ForHead,
        right: &Expr,
        body: &Stmt,
        is_async: bool,
    ) {
        if self.fr().completion {
            self.emit(Op::ClearCompletion);
        }
        self.for_head_rhs(left, right);
        let iter = self.alloc_regs(2);
        self.emit(Op::GetIterator { iter, is_async });
        let (brk, cont, done, on_throw) = (
            self.new_label(),
            self.new_label(),
            self.new_label(),
            self.new_label(),
        );
        self.bind(cont);
        if is_async {
            self.emit(Op::IteratorCallNext { iter });
            self.emit(Op::Await);
            self.emit(Op::AwaitResume);
            self.emit_jump(Op::IteratorResult { done: 0 }, done);
        } else {
            self.emit_jump(Op::IteratorStep { iter, done: 0 }, done);
        }
        self.fs().control.push(Control::Breakable {
            labels,
            kind: BreakKind::Loop,
            brk,
            cont: Some(cont),
            iter: Some((iter, is_async)),
        });
        self.emit_jump(Op::PushHandler(0), on_throw);
        self.fs().control.push(Control::Handler);
        self.for_bind_and_body(left, body);
        self.fs().control.pop();
        self.emit(Op::PopHandler);
        self.fs().control.pop();
        self.jump(cont);
        // A throw from the binding or the body closes the iterator.
        self.bind(on_throw);
        if is_async {
            self.emit_async_close_on_throw(iter);
        } else {
            self.emit(Op::IteratorCloseThrow { iter });
        }
        self.bind(brk);
        self.emit_iterator_close(iter, is_async);
        self.bind(done);
    }

    /// `[exc]`: AsyncIteratorClose with a throw completion, then rethrow.
    pub(crate) fn emit_async_close_on_throw(&mut self, iter: u32) {
        let exc = self.alloc_reg();
        self.emit(Op::StoreReg(exc));
        let (rethrow, ignore) = (self.new_label(), self.new_label());
        self.emit_jump(Op::AsyncIteratorCloseThrowCall { iter, skip: 0 }, rethrow);
        self.emit_jump(Op::PushHandler(0), ignore);
        self.emit(Op::Await);
        self.emit(Op::AwaitResume);
        self.emit(Op::PopHandler);
        self.bind(ignore);
        self.emit(Op::Pop);
        self.bind(rethrow);
        self.emit(Op::LoadReg(exc));
        self.emit(Op::Throw);
    }

    // ----- switch -----

    fn switch(&mut self, labels: Vec<String>, discriminant: &Expr, cases: &[SwitchCase]) {
        if self.fr().completion {
            self.emit(Op::ClearCompletion);
        }
        self.expr(discriminant);
        let reg = self.alloc_reg();
        self.emit(Op::StoreReg(reg));
        let all: Vec<Stmt> = cases.iter().flat_map(|c| c.body.iter().cloned()).collect();
        let decls = lexical_declarations(&all);
        let scoped = !decls.is_empty();
        if scoped {
            let mut scope = Scope::new(ScopeKind::Block);
            for (name, kind) in &decls {
                let flags = match kind {
                    LexKind::Function => 0,
                    LexKind::Const => lexical_flags(true),
                    _ => lexical_flags(false),
                };
                scope.declare(name, flags);
            }
            self.push_scope(scope);
            self.instantiate_block_functions(&all);
        }
        let brk = self.new_label();
        self.fs().control.push(Control::Breakable {
            labels,
            kind: BreakKind::Switch,
            brk,
            cont: None,
            iter: None,
        });
        let bodies: Vec<_> = cases.iter().map(|_| self.new_label()).collect();
        let mut default = None;
        for (i, case) in cases.iter().enumerate() {
            match &case.test {
                Some(test) => {
                    self.emit(Op::LoadReg(reg));
                    self.expr(test);
                    self.emit(Op::StrictEq);
                    self.emit_jump(Op::JumpIfTrue(0), bodies[i]);
                }
                None => default = Some(i),
            }
        }
        match default {
            Some(i) => self.jump(bodies[i]),
            None => self.jump(brk),
        }
        let using = using_kind(&all);
        let mut body = |c: &mut Self| {
            for (i, case) in cases.iter().enumerate() {
                c.bind(bodies[i]);
                c.stmts(&case.body);
            }
        };
        match using {
            Some(is_async) => self.using_block(is_async, body),
            None => body(self),
        }
        self.fs().control.pop();
        self.bind(brk);
        if scoped {
            self.pop_scope(true);
        }
    }

    // ----- try -----

    fn try_stmt(
        &mut self,
        block: &[Stmt],
        handler: Option<&CatchClause>,
        finalizer: Option<&[Stmt]>,
    ) {
        let completion = self.fr().completion;
        if completion {
            self.emit(Op::ClearCompletion);
        }
        let mut fin = None;
        if finalizer.is_some() {
            let kind_reg = self.alloc_reg();
            let value_reg = self.alloc_reg();
            let (entry, on_throw) = (self.new_label(), self.new_label());
            self.emit_jump(Op::PushHandler(0), on_throw);
            self.fs().control.push(Control::Finally(FinallyInfo {
                kind_reg,
                value_reg,
                entry,
                exits: Vec::new(),
            }));
            fin = Some((kind_reg, value_reg, entry, on_throw));
        }
        match handler {
            Some(h) => {
                let (catch, after) = (self.new_label(), self.new_label());
                self.emit_jump(Op::PushHandler(0), catch);
                self.fs().control.push(Control::Handler);
                self.block(block);
                self.fs().control.pop();
                self.emit(Op::PopHandler);
                self.jump(after);
                self.bind(catch);
                if completion {
                    self.emit(Op::ClearCompletion);
                }
                self.catch_clause(h);
                self.bind(after);
            }
            None => self.block(block),
        }
        let Some((kind_reg, value_reg, entry, on_throw)) = fin else {
            return;
        };
        let Some(Control::Finally(info)) = self.fs().control.pop() else {
            unreachable!("finally control entry");
        };
        self.emit(Op::PopHandler);
        self.emit(Op::Int(0));
        self.emit(Op::StoreReg(kind_reg));
        self.jump(entry);
        self.bind(on_throw);
        self.emit(Op::StoreReg(value_reg));
        self.emit(Op::Int(1));
        self.emit(Op::StoreReg(kind_reg));
        self.bind(entry);
        let saved = if completion {
            let r = self.alloc_reg();
            self.emit(Op::LoadCompletion);
            self.emit(Op::StoreReg(r));
            Some(r)
        } else {
            None
        };
        self.block(finalizer.unwrap_or(&[]));
        if let Some(r) = saved {
            self.emit(Op::LoadReg(r));
            self.emit(Op::SetCompletion);
        }
        let table = self.new_exit_table();
        self.emit(Op::EndFinally {
            kind: kind_reg,
            value: value_reg,
            table,
        });
        let end = self.new_label();
        self.jump(end);
        self.emit_exit_trampolines(&info, table);
        self.bind(end);
    }

    /// `[exc]`: the catch parameter scope and the catch block.
    fn catch_clause(&mut self, h: &CatchClause) {
        match &h.param {
            None => {
                self.emit(Op::Pop);
                self.block(&h.body);
            }
            Some(param) => {
                let mut scope = Scope::new(ScopeKind::Catch);
                let mut names = Vec::new();
                bound_names(param, &mut names);
                for n in &names {
                    scope.declare(n, 0);
                }
                scope.simple_catch = matches!(param.kind, PatternKind::Ident(_));
                self.push_scope(scope);
                self.pattern(param, BindMode::Init);
                self.block(&h.body);
                self.pop_scope(true);
            }
        }
    }

    // ----- prologues -----

    /// GlobalDeclarationInstantiation data and the global code prologue.
    pub(crate) fn global_prologue(&mut self, program: &Program) {
        let decls = self.collect_declarations(&program.body, &program.scope.var_names);
        let fs = self.fs();
        fs.decls.push(decls);
        let idx = (fs.decls.len() - 1) as u32;
        self.emit(Op::GlobalDeclInit(idx));
    }

    /// Var names, functions and lexical declarations of a script or a
    /// sloppy eval body (the function code is compiled here).
    fn collect_declarations(&mut self, body: &[Stmt], var_names: &[String]) -> Declarations {
        let functions = functions_to_initialize(&hoisted_functions(body));
        let function_names: Vec<String> = functions
            .iter()
            .filter_map(|f| f.name.as_ref().map(|n| n.name.clone()))
            .collect();
        let mut decls = Declarations::default();
        for f in &functions {
            let idx = self.function(f, super::func::FnName::Own, false);
            let name = f.name.as_ref().map(|n| n.name.as_str()).unwrap_or("");
            let a = self.agent.heap.intern_str(name);
            decls.functions.push((a, idx));
        }
        for v in var_names {
            if function_names.contains(v) {
                continue;
            }
            let a = self.agent.heap.intern_str(v);
            decls.var_names.push(a);
        }
        for (name, kind) in top_lexical_declarations(body) {
            let a = self.agent.heap.intern_str(&name);
            decls.lexical.push((a, kind == LexKind::Const));
        }
        decls
    }

    /// The prologue of eval code: strict eval code gets one scope with
    /// every declaration; sloppy eval code keeps its lexical declarations
    /// in a scope and instantiates vars and functions in the caller's
    /// variable environment.
    pub(crate) fn eval_prologue(&mut self, program: &Program) {
        let lexical = top_lexical_declarations(&program.body);
        if program.strict {
            let mut scope = Scope::new(ScopeKind::Eval);
            scope.var_scope = true;
            for v in &program.scope.var_names {
                scope.declare(v, 0);
            }
            for (name, kind) in &lexical {
                scope.declare(name, lexical_flags(*kind == LexKind::Const));
            }
            self.push_scope(scope);
            let functions = functions_to_initialize(&hoisted_functions(&program.body));
            for f in functions {
                let idx = self.function(f, super::func::FnName::Own, false);
                self.emit(Op::Closure(idx));
                let name = f.name.as_ref().map(|n| n.name.clone()).unwrap_or_default();
                self.init_name(&name);
            }
            return;
        }
        let mut scope = Scope::new(ScopeKind::Eval);
        for (name, kind) in &lexical {
            scope.declare(name, lexical_flags(*kind == LexKind::Const));
        }
        // The variable environment of the caller receives new bindings.
        scope.tainted = true;
        self.push_scope(scope);
        let decls = self.collect_declarations(&program.body, &program.scope.var_names);
        let fs = self.fs();
        fs.decls.push(decls);
        let idx = (fs.decls.len() - 1) as u32;
        self.emit(Op::EvalDeclInit(idx));
    }

    /// Whether the innermost code is global code (its top-level names are
    /// global bindings).
    pub(crate) fn at_global_level(&self) -> bool {
        self.fr().kind == CodeKind::Script
    }

    /// Binds a single identifier pattern; used by parameters and `catch`.
    pub(crate) fn bind_simple(&mut self, target: &Pattern, mode: BindMode) {
        self.pattern(target, mode);
    }
}
