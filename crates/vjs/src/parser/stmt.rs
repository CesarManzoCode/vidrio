//! Statements and declarations (ECMA-262 §14), with the Annex B.3
//! extensions for sloppy mode.

use super::func::FnSite;
use super::{Bind, Cover, Label, LabelKind, Parser, R, SIMPLE_CATCH, SWITCH, TOP, Usage};
use crate::ast::{
    CatchClause, Declarator, Expr, ExprKind, ForHead, ForInit, Name, Pattern, PatternKind, Stmt,
    StmtKind, SwitchCase, VarDecl, VarKind,
};
use crate::lexer::T;

/// Where a statement appears: in a StatementList (declarations allowed),
/// as the body of `if`, as the item of a label, or as any other
/// single-statement body (loops, `with`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ctx {
    List,
    If,
    /// `in_list`: the label (chain) itself is in a StatementList.
    Label {
        in_list: bool,
    },
    Body,
}

impl<'a> Parser<'a> {
    fn stmt(&self, start: u32, kind: StmtKind) -> Stmt {
        Stmt {
            span: self.span_from(start),
            kind,
        }
    }

    pub(crate) fn parse_statement_list_item(&mut self) -> R<Stmt> {
        self.parse_statement(Ctx::List)
    }

    pub(crate) fn parse_statement(&mut self, ctx: Ctx) -> R<Stmt> {
        let start = self.start();
        match self.tok.kind {
            T::LBrace => {
                let body = self.parse_block()?;
                return Ok(self.stmt(start, StmtKind::Block(body)));
            }
            T::Semi => {
                self.next()?;
                return Ok(self.stmt(start, StmtKind::Empty));
            }
            T::Name if !self.tok.escaped => {
                let word = self.tok.name().to_string();
                match word.as_str() {
                    "var" => {
                        self.next()?;
                        let decl = self.parse_var_decl(start, VarKind::Var, false)?;
                        self.semicolon()?;
                        return Ok(self.stmt(start, StmtKind::Var(decl)));
                    }
                    "let" | "const" if word == "const" || self.is_let_decl(ctx) => {
                        if ctx != Ctx::List {
                            return Err(self.error(
                                self.tok.span,
                                "lexical declarations are not allowed in a single-statement context",
                            ));
                        }
                        let kind = if word == "let" {
                            VarKind::Let
                        } else {
                            VarKind::Const
                        };
                        self.next()?;
                        let decl = self.parse_var_decl(start, kind, false)?;
                        self.semicolon()?;
                        return Ok(self.stmt(start, StmtKind::Var(decl)));
                    }
                    "function" => return self.parse_function_statement(start, ctx, false),
                    "async" if self.is_async_function() => {
                        if ctx != Ctx::List {
                            return Err(self.error(
                                self.tok.span,
                                "async functions can only be declared in a statement list",
                            ));
                        }
                        self.next()?;
                        return self.parse_function_statement(start, ctx, true);
                    }
                    "class" => {
                        if ctx != Ctx::List {
                            return Err(self.error(
                                self.tok.span,
                                "classes can only be declared in a statement list",
                            ));
                        }
                        let class = self.parse_class(start, true, false)?;
                        return Ok(self.stmt(start, StmtKind::Class(Box::new(class))));
                    }
                    "if" => return self.parse_if(start),
                    "for" => return self.parse_for(start),
                    "while" => return self.parse_while(start),
                    "do" => return self.parse_do_while(start),
                    "return" => return self.parse_return(start),
                    "break" => return self.parse_break_continue(start, true),
                    "continue" => return self.parse_break_continue(start, false),
                    "throw" => return self.parse_throw(start),
                    "try" => return self.parse_try(start),
                    "switch" => return self.parse_switch(start),
                    "with" => return self.parse_with(start),
                    "debugger" => {
                        self.next()?;
                        self.semicolon()?;
                        return Ok(self.stmt(start, StmtKind::Debugger));
                    }
                    "import" => {
                        let is_expr = self
                            .peek()
                            .is_some_and(|t| matches!(t.kind, T::LParen | T::Dot));
                        if !is_expr {
                            if ctx != Ctx::List || !self.module || self.scopes.len() != 1 {
                                return Err(self.error(
                                    self.tok.span,
                                    "import declarations may only appear at the top level of a module",
                                ));
                            }
                            return self.parse_import_decl(start);
                        }
                    }
                    "export" => {
                        if ctx != Ctx::List || !self.module || self.scopes.len() != 1 {
                            return Err(self.error(
                                self.tok.span,
                                "export declarations may only appear at the top level of a module",
                            ));
                        }
                        return self.parse_export_decl(start);
                    }
                    "using" if ctx == Ctx::List && self.is_using(false) => {
                        return self.parse_using_statement(start, false);
                    }
                    "await" if ctx == Ctx::List && self.is_await_using(false) => {
                        return self.parse_using_statement(start, true);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if self.is(T::Name) && self.peek().is_some_and(|t| t.kind == T::Colon) {
            return self.parse_labeled(start, ctx);
        }
        let expr = self.parse_expression(false)?;
        self.semicolon()?;
        Ok(self.stmt(start, StmtKind::Expr(Box::new(expr))))
    }

    /// `async [no LineTerminator here] function`.
    pub(crate) fn is_async_function(&self) -> bool {
        self.is_kw("async")
            && self.peek().is_some_and(|t| {
                t.kind == T::Name && !t.escaped && t.name() == "function" && !t.nl_before
            })
    }

    /// `let` starts a LexicalDeclaration here (ECMA-262 lookahead rules:
    /// `let [` always does; elsewhere only in a StatementList).
    pub(crate) fn is_let_decl(&self, ctx: Ctx) -> bool {
        let Some(next) = self.peek() else {
            return false;
        };
        if next.kind == T::LBracket {
            return true;
        }
        if ctx != Ctx::List {
            return false;
        }
        match next.kind {
            T::LBrace => true,
            T::Name => next.escaped || !matches!(next.name(), "in" | "instanceof"),
            _ => false,
        }
    }

    /// `using [no LineTerminator here] BindingIdentifier`.
    pub(crate) fn is_using(&self, is_for: bool) -> bool {
        if !self.is_kw("using") {
            return false;
        }
        let Some(next) = self.peek() else {
            return false;
        };
        if next.nl_before || next.kind != T::Name {
            return false;
        }
        if !next.escaped && matches!(next.name(), "in" | "instanceof") {
            return false;
        }
        !(is_for && !next.escaped && next.name() == "of")
    }

    /// `await [no LineTerminator here] using [no LineTerminator here]
    /// BindingIdentifier`.
    pub(crate) fn is_await_using(&self, _is_for: bool) -> bool {
        if !self.is_kw("await") {
            return false;
        }
        let Some((using, binding)) = self.peek2() else {
            return false;
        };
        if using.nl_before || using.kind != T::Name || using.escaped || using.name() != "using" {
            return false;
        }
        if binding.nl_before || binding.kind != T::Name {
            return false;
        }
        binding.escaped || !matches!(binding.name(), "in" | "instanceof")
    }

    fn parse_function_statement(&mut self, start: u32, ctx: Ctx, is_async: bool) -> R<Stmt> {
        let allowed = match ctx {
            Ctx::List => true,
            Ctx::If | Ctx::Label { in_list: true } => !self.strict && !is_async,
            _ => false,
        };
        if !allowed {
            return Err(self.error(
                self.tok.span,
                "function declarations are not allowed in this context",
            ));
        }
        if ctx != Ctx::List && self.peek().is_some_and(|t| t.kind == T::Star) {
            return Err(self.error(
                self.tok.span,
                "generator declarations are not allowed in this context",
            ));
        }
        let function = self.parse_function(
            start,
            is_async,
            FnSite::Declaration {
                hanging: ctx == Ctx::If,
                default_export: false,
            },
        )?;
        Ok(self.stmt(start, StmtKind::Function(Box::new(function))))
    }

    /// `{ StatementList }` in a new scope.
    pub(crate) fn parse_block(&mut self) -> R<Vec<Stmt>> {
        self.enter_scope(0);
        let body = self.parse_block_body()?;
        self.exit_scope();
        Ok(body)
    }

    fn parse_block_body(&mut self) -> R<Vec<Stmt>> {
        self.expect(T::LBrace)?;
        let mut body = Vec::new();
        while !self.eat(T::RBrace)? {
            body.push(self.parse_statement_list_item()?);
        }
        Ok(body)
    }

    fn parse_paren_expression(&mut self) -> R<Expr> {
        self.expect(T::LParen)?;
        let e = self.parse_expression(false)?;
        self.expect(T::RParen)?;
        Ok(e)
    }

    fn push_loop_label(&mut self, start: u32) {
        self.labels.push(Label {
            name: None,
            kind: LabelKind::Loop,
            stmt_start: start,
        });
    }

    fn parse_if(&mut self, start: u32) -> R<Stmt> {
        self.next()?;
        let test = self.parse_paren_expression()?;
        let consequent = self.parse_statement(Ctx::If)?;
        let alternate = if self.eat_kw("else")? {
            Some(Box::new(self.parse_statement(Ctx::If)?))
        } else {
            None
        };
        Ok(self.stmt(
            start,
            StmtKind::If {
                test: Box::new(test),
                consequent: Box::new(consequent),
                alternate,
            },
        ))
    }

    fn parse_while(&mut self, start: u32) -> R<Stmt> {
        self.next()?;
        let test = self.parse_paren_expression()?;
        self.push_loop_label(start);
        let body = self.parse_statement(Ctx::Body)?;
        self.labels.pop();
        Ok(self.stmt(
            start,
            StmtKind::While {
                test: Box::new(test),
                body: Box::new(body),
            },
        ))
    }

    fn parse_do_while(&mut self, start: u32) -> R<Stmt> {
        self.next()?;
        self.push_loop_label(start);
        let body = self.parse_statement(Ctx::Body)?;
        self.labels.pop();
        self.expect_kw("while")?;
        let test = self.parse_paren_expression()?;
        // A semicolon is inserted after do-while even without a line break.
        self.eat(T::Semi)?;
        Ok(self.stmt(
            start,
            StmtKind::DoWhile {
                body: Box::new(body),
                test: Box::new(test),
            },
        ))
    }

    fn parse_for(&mut self, start: u32) -> R<Stmt> {
        self.next()?;
        let await_at = if self.is_kw("await") && self.can_await() {
            let pos = self.start();
            self.next()?;
            Some(pos)
        } else {
            None
        };
        self.push_loop_label(start);
        self.enter_scope(0);
        self.expect(T::LParen)?;
        if self.is(T::Semi) {
            if let Some(pos) = await_at {
                return Err(self.error_at(pos, "for await requires an of clause"));
            }
            return self.parse_for_loop(start, None);
        }
        let init_start = self.start();
        let is_let = self.is_kw("let") && self.is_let_decl(Ctx::List);
        if self.is_kw("var") || self.is_kw("const") || is_let {
            let kind = match self.tok.name() {
                "var" => VarKind::Var,
                "let" => VarKind::Let,
                _ => VarKind::Const,
            };
            self.next()?;
            let decl = self.parse_var_decl(init_start, kind, true)?;
            return self.parse_for_after_decl(start, decl, await_at);
        }
        let using = if self.is_using(true) {
            Some(VarKind::Using)
        } else if self.is_await_using(true) {
            Some(VarKind::AwaitUsing)
        } else {
            None
        };
        if let Some(kind) = using {
            if kind == VarKind::AwaitUsing {
                if !self.can_await() {
                    return Err(
                        self.error(self.tok.span, "await using is only valid in async contexts")
                    );
                }
                self.next()?;
            }
            self.next()?;
            let decl = self.parse_var_decl(init_start, kind, true)?;
            return self.parse_for_after_decl(start, decl, await_at);
        }
        let starts_with_let = self.is_kw("let");
        let starts_with_async = self.is_kw("async");
        let mut cover = Cover::default();
        let init = if await_at.is_some() {
            self.parse_subscripts_expr(Some(&mut cover), true)?
        } else {
            self.parse_expression_cover(true, &mut cover, false)?
        };
        let is_of = self.is_kw("of");
        if self.is_kw("in") || is_of {
            if await_at.is_some() && !is_of {
                return Err(self.error(self.tok.span, "for await requires an of clause"));
            }
            if is_of
                && await_at.is_none()
                && starts_with_async
                && init.span.start == init_start
                && matches!(&init.kind, ExprKind::Ident(n) if n == "async")
            {
                return Err(self.error(init.span, "the left side of for-of cannot be 'async'"));
            }
            if starts_with_let && is_of {
                return Err(
                    self.error(init.span, "the left side of for-of cannot start with 'let'")
                );
            }
            let target = self.to_assign_pattern(init, Some(&cover))?;
            return self.parse_for_in_of(start, ForHead::Target(target), is_of, await_at.is_some());
        }
        self.check_expression_errors(&cover)?;
        if let Some(pos) = await_at {
            return Err(self.error_at(pos, "for await requires an of clause"));
        }
        self.parse_for_loop(start, Some(ForInit::Expr(Box::new(init))))
    }

    fn parse_for_after_decl(
        &mut self,
        start: u32,
        decl: VarDecl,
        await_at: Option<u32>,
    ) -> R<Stmt> {
        let is_of = self.is_kw("of");
        if (self.is_kw("in") || is_of) && decl.declarations.len() == 1 {
            if !is_of && await_at.is_some() {
                return Err(self.error(self.tok.span, "for await requires an of clause"));
            }
            let declarator = &decl.declarations[0];
            if declarator.init.is_some()
                && (is_of
                    || self.strict
                    || decl.kind != VarKind::Var
                    || !matches!(declarator.target.kind, PatternKind::Ident(_)))
            {
                return Err(self.error(
                    declarator.span,
                    "for-in/of loop variable declaration may not have an initializer",
                ));
            }
            if !is_of && matches!(decl.kind, VarKind::Using | VarKind::AwaitUsing) {
                return Err(self.error(decl.span, "using declarations are not allowed in for-in"));
            }
            return self.parse_for_in_of(start, ForHead::Var(decl), is_of, await_at.is_some());
        }
        if let Some(pos) = await_at {
            return Err(self.error_at(pos, "for await requires an of clause"));
        }
        self.parse_for_loop(start, Some(ForInit::Var(decl)))
    }

    fn parse_for_in_of(
        &mut self,
        start: u32,
        left: ForHead,
        is_of: bool,
        is_await: bool,
    ) -> R<Stmt> {
        self.next()?;
        let right = if is_of {
            self.parse_assign(false)?
        } else {
            self.parse_expression(false)?
        };
        self.expect(T::RParen)?;
        let body = self.parse_statement(Ctx::Body)?;
        self.exit_scope();
        self.labels.pop();
        let kind = if is_of {
            StmtKind::ForOf {
                left,
                right: Box::new(right),
                body: Box::new(body),
                is_await,
            }
        } else {
            StmtKind::ForIn {
                left,
                right: Box::new(right),
                body: Box::new(body),
            }
        };
        Ok(self.stmt(start, kind))
    }

    fn parse_for_loop(&mut self, start: u32, init: Option<ForInit>) -> R<Stmt> {
        self.expect(T::Semi)?;
        let test = if self.is(T::Semi) {
            None
        } else {
            Some(Box::new(self.parse_expression(false)?))
        };
        self.expect(T::Semi)?;
        let update = if self.is(T::RParen) {
            None
        } else {
            Some(Box::new(self.parse_expression(false)?))
        };
        self.expect(T::RParen)?;
        let body = self.parse_statement(Ctx::Body)?;
        self.exit_scope();
        self.labels.pop();
        Ok(self.stmt(
            start,
            StmtKind::For {
                init,
                test,
                update,
                body: Box::new(body),
            },
        ))
    }

    /// The declarators after `var`, `let`, `const`, `using` or `await
    /// using`. In a for head (`in_for`) `in` is not an operator and the
    /// initializer may be missing before `in`/`of`.
    pub(crate) fn parse_var_decl(&mut self, start: u32, kind: VarKind, in_for: bool) -> R<VarDecl> {
        let using = matches!(kind, VarKind::Using | VarKind::AwaitUsing);
        let mut declarations = Vec::new();
        loop {
            let decl_start = self.start();
            let target = if using {
                if self.is(T::LBracket) || self.is(T::LBrace) {
                    return Err(self.error(
                        self.tok.span,
                        "using declarations cannot have binding patterns",
                    ));
                }
                let (name, span) = self.parse_ident(Usage::Binding)?;
                Pattern {
                    span,
                    kind: PatternKind::Ident(name),
                }
            } else {
                self.parse_binding_target()?
            };
            let bind = if kind == VarKind::Var {
                Bind::Var
            } else {
                Bind::Lexical
            };
            self.declare_pattern(&target, bind)?;
            let init = if self.eat(T::Assign)? {
                Some(Box::new(self.parse_assign(in_for)?))
            } else {
                let in_or_of = in_for && (self.is_kw("in") || self.is_kw("of"));
                if !in_or_of {
                    if kind == VarKind::Const {
                        return Err(
                            self.error(target.span, "missing initializer in const declaration")
                        );
                    }
                    if using {
                        return Err(
                            self.error(target.span, "missing initializer in using declaration")
                        );
                    }
                    if !matches!(target.kind, PatternKind::Ident(_)) {
                        return Err(self.error(
                            target.span,
                            "missing initializer in destructuring declaration",
                        ));
                    }
                }
                None
            };
            declarations.push(Declarator {
                span: self.span_from(decl_start),
                target,
                init,
            });
            if !self.eat(T::Comma)? {
                break;
            }
        }
        Ok(VarDecl {
            span: self.span_from(start),
            kind,
            declarations,
        })
    }

    fn parse_using_statement(&mut self, start: u32, is_await: bool) -> R<Stmt> {
        let flags = self.scope_flags();
        if flags & TOP != 0 && !self.module {
            return Err(self.error(
                self.tok.span,
                "using declarations are not allowed at the top level of a script",
            ));
        }
        if flags & SWITCH != 0 {
            return Err(self.error(
                self.tok.span,
                "using declarations are not allowed directly in a case clause",
            ));
        }
        let kind = if is_await {
            if !self.can_await() {
                return Err(
                    self.error(self.tok.span, "await using is only valid in async contexts")
                );
            }
            self.next()?;
            VarKind::AwaitUsing
        } else {
            VarKind::Using
        };
        self.next()?;
        let decl = self.parse_var_decl(start, kind, false)?;
        self.semicolon()?;
        Ok(self.stmt(start, StmtKind::Var(decl)))
    }

    fn parse_return(&mut self, start: u32) -> R<Stmt> {
        if !self.in_function() {
            return Err(self.error(self.tok.span, "return is only valid in functions"));
        }
        self.next()?;
        let argument = if self.eat(T::Semi)? || self.can_insert_semicolon() {
            None
        } else {
            let e = self.parse_expression(false)?;
            self.semicolon()?;
            Some(Box::new(e))
        };
        Ok(self.stmt(start, StmtKind::Return(argument)))
    }

    fn parse_break_continue(&mut self, start: u32, is_break: bool) -> R<Stmt> {
        let keyword_span = self.tok.span;
        self.next()?;
        let label = if self.is(T::Semi) || self.can_insert_semicolon() {
            None
        } else if self.is(T::Name) {
            let (name, span) = self.parse_ident(Usage::Label)?;
            Some(Name { span, name })
        } else {
            return Err(self.unexpected());
        };
        self.semicolon()?;
        let found = self.labels.iter().any(|l| match &label {
            None => l.kind != LabelKind::Other && (is_break || l.kind == LabelKind::Loop),
            Some(label) => {
                l.name.as_deref() == Some(label.name.as_str())
                    && (is_break || l.kind == LabelKind::Loop)
            }
        });
        if !found {
            let message = match (&label, is_break) {
                (Some(label), _) => format!("undefined or invalid label '{}'", label.name),
                (None, true) => "break must be inside a loop or switch".to_string(),
                (None, false) => "continue must be inside a loop".to_string(),
            };
            return Err(self.error(keyword_span, &message));
        }
        let kind = if is_break {
            StmtKind::Break(label)
        } else {
            StmtKind::Continue(label)
        };
        Ok(self.stmt(start, kind))
    }

    fn parse_throw(&mut self, start: u32) -> R<Stmt> {
        self.next()?;
        if self.tok.nl_before {
            return Err(self.error(self.tok.span, "no line break is allowed after throw"));
        }
        let argument = self.parse_expression(false)?;
        self.semicolon()?;
        Ok(self.stmt(start, StmtKind::Throw(Box::new(argument))))
    }

    fn parse_try(&mut self, start: u32) -> R<Stmt> {
        self.next()?;
        let block = self.parse_block()?;
        let handler = if self.is_kw("catch") {
            let catch_start = self.start();
            self.next()?;
            let (param, body) = if self.eat(T::LParen)? {
                let param = self.parse_binding_target()?;
                let simple = matches!(param.kind, PatternKind::Ident(_));
                self.enter_scope(if simple { SIMPLE_CATCH } else { 0 });
                self.declare_pattern(
                    &param,
                    if simple {
                        Bind::SimpleCatch
                    } else {
                        Bind::Lexical
                    },
                )?;
                self.expect(T::RParen)?;
                let body = self.parse_block_body()?;
                self.exit_scope();
                (Some(param), body)
            } else {
                self.enter_scope(0);
                let body = self.parse_block_body()?;
                self.exit_scope();
                (None, body)
            };
            Some(CatchClause {
                span: self.span_from(catch_start),
                param,
                body,
            })
        } else {
            None
        };
        let finalizer = if self.eat_kw("finally")? {
            Some(self.parse_block()?)
        } else {
            None
        };
        if handler.is_none() && finalizer.is_none() {
            return Err(self.error(
                self.span_from(start),
                "try requires a catch or finally clause",
            ));
        }
        Ok(self.stmt(
            start,
            StmtKind::Try {
                block,
                handler,
                finalizer,
            },
        ))
    }

    fn parse_switch(&mut self, start: u32) -> R<Stmt> {
        self.next()?;
        let discriminant = self.parse_paren_expression()?;
        self.expect(T::LBrace)?;
        self.labels.push(Label {
            name: None,
            kind: LabelKind::Switch,
            stmt_start: start,
        });
        self.enter_scope(SWITCH);
        let mut cases = Vec::new();
        let mut saw_default = false;
        while !self.is(T::RBrace) {
            let case_start = self.start();
            let test = if self.eat_kw("case")? {
                Some(Box::new(self.parse_expression(false)?))
            } else if self.is_kw("default") {
                if saw_default {
                    return Err(
                        self.error(self.tok.span, "a switch can only have one default clause")
                    );
                }
                saw_default = true;
                self.next()?;
                None
            } else {
                return Err(self.unexpected());
            };
            self.expect(T::Colon)?;
            let mut body = Vec::new();
            while !self.is(T::RBrace) && !self.is_kw("case") && !self.is_kw("default") {
                body.push(self.parse_statement_list_item()?);
            }
            cases.push(SwitchCase {
                span: self.span_from(case_start),
                test,
                body,
            });
        }
        self.next()?;
        self.exit_scope();
        self.labels.pop();
        Ok(self.stmt(
            start,
            StmtKind::Switch {
                discriminant: Box::new(discriminant),
                cases,
            },
        ))
    }

    fn parse_with(&mut self, start: u32) -> R<Stmt> {
        if self.strict {
            return Err(self.error(self.tok.span, "with is not allowed in strict mode"));
        }
        self.next()?;
        let object = self.parse_paren_expression()?;
        let body = self.parse_statement(Ctx::Body)?;
        Ok(self.stmt(
            start,
            StmtKind::With {
                object: Box::new(object),
                body: Box::new(body),
            },
        ))
    }

    fn parse_labeled(&mut self, start: u32, ctx: Ctx) -> R<Stmt> {
        let (name, span) = self.parse_ident(Usage::Label)?;
        self.expect(T::Colon)?;
        if self
            .labels
            .iter()
            .any(|l| l.name.as_deref() == Some(name.as_str()))
        {
            return Err(self.error(span, &format!("label '{name}' has already been declared")));
        }
        let kind = if self.is_kw("do") || self.is_kw("for") || self.is_kw("while") {
            LabelKind::Loop
        } else if self.is_kw("switch") {
            LabelKind::Switch
        } else {
            LabelKind::Other
        };
        let body_start = self.start();
        for label in self.labels.iter_mut().rev() {
            if label.stmt_start == start {
                label.stmt_start = body_start;
                label.kind = kind;
            } else {
                break;
            }
        }
        self.labels.push(Label {
            name: Some(name.clone()),
            kind,
            stmt_start: body_start,
        });
        let body_ctx = match ctx {
            Ctx::List => Ctx::Label { in_list: true },
            Ctx::Label { in_list } => Ctx::Label { in_list },
            _ => Ctx::Label { in_list: false },
        };
        let body = self.parse_statement(body_ctx)?;
        self.labels.pop();
        Ok(self.stmt(
            start,
            StmtKind::Labeled {
                label: Name { span, name },
                body: Box::new(body),
            },
        ))
    }
}
