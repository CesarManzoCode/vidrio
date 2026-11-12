//! Recursive-descent parser for ECMAScript scripts and modules (ECMA-262
//! 2026 plus Annex B), producing the AST of [`crate::ast`] and reporting
//! every early error as a [`SyntaxError`].
//!
//! Cover grammars (parenthesized expressions vs. arrow parameters, object
//! and array literals vs. destructuring patterns, `async(...)` calls vs.
//! async arrow heads) are parsed as expressions and converted afterwards;
//! errors that only apply when the cover is *not* reinterpreted are kept in
//! a [`Cover`] record until the decision is made. Declarations are checked
//! against a stack of scopes as they are parsed.

mod expr;
mod func;
mod module;
mod stmt;

use std::collections::{HashMap, HashSet};

use crate::ast::{Goal, Program, ScopeInfo, Span, Stmt, SyntaxError, line_column};
use crate::lexer::{Lexer, T, Token, Value};

pub type R<T> = Result<T, SyntaxError>;

/// Early-error hook for RegularExpressionLiteral bodies (ECMA-262 §22.2.1):
/// receives the pattern and the (already validated) flags and returns an
/// error message if the pattern is not valid. Supplied by the RegExp engine.
pub type RegExpValidator = fn(pattern: &str, flags: &str) -> Result<(), String>;

#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    /// Start in strict mode (direct eval from strict code).
    pub strict: bool,
    /// Validates regular expression patterns; `None` accepts any body.
    pub regexp_validator: Option<RegExpValidator>,
    /// Set when parsing the code of a direct `eval`.
    pub eval: Option<EvalContext>,
}

/// What the code around a direct `eval` allows (ECMA-262 §19.2.1.1,
/// PerformEval steps on `inFunction`, `inMethod`, `inDerivedConstructor`
/// and `inClassFieldInitializer`).
#[derive(Clone, Debug, Default)]
pub struct EvalContext {
    pub new_target: bool,
    pub super_property: bool,
    pub super_call: bool,
    pub class_field_initializer: bool,
    /// Private names declared by the enclosing classes.
    pub private_names: Vec<String>,
}

/// Kind of function built by the `Function`, `GeneratorFunction`,
/// `AsyncFunction` and `AsyncGeneratorFunction` constructors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicFunctionKind {
    Normal,
    Generator,
    Async,
    AsyncGenerator,
}

/// Parses `source` with the Script goal.
pub fn parse_script(source: &str, options: &ParseOptions) -> R<Program> {
    Parser::new(source, Goal::Script, options, true)?.parse_program()
}

/// Parses `source` with the Module goal.
pub fn parse_module(source: &str, options: &ParseOptions) -> R<Program> {
    Parser::new(source, Goal::Module, options, true)?.parse_program()
}

/// CreateDynamicFunction (ECMA-262 §20.2.1.1.1): checks that `params` is a
/// FormalParameters and `body` a FunctionBody on their own, then parses the
/// assembled source text. Returns the function and that source text, whose
/// whole range is the function's `[[SourceText]]`.
pub fn parse_dynamic_function(
    kind: DynamicFunctionKind,
    params: &str,
    body: &str,
    options: &ParseOptions,
) -> R<(crate::ast::Function, String)> {
    let (is_async, is_generator) = match kind {
        DynamicFunctionKind::Normal => (false, false),
        DynamicFunctionKind::Generator => (false, true),
        DynamicFunctionKind::Async => (true, false),
        DynamicFunctionKind::AsyncGenerator => (true, true),
    };
    let mut flags = FUNCTION;
    if is_async {
        flags |= ASYNC;
    }
    if is_generator {
        flags |= GENERATOR;
    }
    let mut p = Parser::new(params, Goal::Script, options, false)?;
    p.check_dynamic_params(flags)?;
    let mut p = Parser::new(body, Goal::Script, options, false)?;
    p.check_dynamic_body(flags)?;
    let prefix = match kind {
        DynamicFunctionKind::Normal => "function",
        DynamicFunctionKind::Generator => "function*",
        DynamicFunctionKind::Async => "async function",
        DynamicFunctionKind::AsyncGenerator => "async function*",
    };
    let source = format!("{prefix} anonymous({params}\n) {{\n{body}\n}}");
    let function = Parser::new(&source, Goal::Script, options, false)?.parse_dynamic_source()?;
    Ok((function, source))
}

// Scope flags.
pub(crate) const TOP: u32 = 1;
pub(crate) const FUNCTION: u32 = 2;
pub(crate) const ASYNC: u32 = 4;
pub(crate) const GENERATOR: u32 = 8;
pub(crate) const ARROW: u32 = 16;
pub(crate) const SIMPLE_CATCH: u32 = 32;
pub(crate) const SUPER: u32 = 64;
pub(crate) const DIRECT_SUPER: u32 = 128;
pub(crate) const STATIC_BLOCK: u32 = 256;
pub(crate) const FIELD_INIT: u32 = 512;
pub(crate) const SWITCH: u32 = 1024;
/// Scopes that receive `var` declarations.
pub(crate) const VAR_SCOPE: u32 = TOP | FUNCTION | STATIC_BLOCK | FIELD_INIT;

pub(crate) struct Scope {
    flags: u32,
    var: HashSet<String>,
    lexical: HashSet<String>,
    functions: HashSet<String>,
    /// The parameter of a `catch (e)` with a simple binding.
    catch_param: Option<String>,
    info: ScopeInfo,
}

impl Scope {
    fn new(flags: u32) -> Scope {
        Scope {
            flags,
            var: HashSet::new(),
            lexical: HashSet::new(),
            functions: HashSet::new(),
            catch_param: None,
            info: ScopeInfo::default(),
        }
    }
}

/// How a name is bound (the binding types of Acorn's scope tracker).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bind {
    Var,
    /// A formal parameter: var-like, not reported in `var_names`.
    Param,
    Lexical,
    /// A function declaration in sloppy mode (Annex B.3.2.4 duplicates).
    Function,
    SimpleCatch,
}

/// Where an identifier appears, for the reserved-word early errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Usage {
    Reference,
    Binding,
    Label,
}

/// Early errors of a cover grammar, recorded while it is parsed as an
/// expression and raised only if it stays an expression (or, for the
/// pattern errors, if it becomes a pattern).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Cover {
    /// CoverInitializedName `{a = 1}`: valid only in a pattern.
    pub shorthand_assign: Option<u32>,
    /// A comma after a rest/spread element: invalid in a pattern.
    pub trailing_comma: Option<u32>,
    /// A parenthesized non-simple target: invalid in an assignment pattern.
    pub paren_assign: Option<u32>,
    /// Any parenthesized expression: invalid in a binding pattern.
    pub paren_bind: Option<u32>,
    /// A second `__proto__: v`: valid only in a pattern.
    pub double_proto: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LabelKind {
    Loop,
    Switch,
    Other,
}

pub(crate) struct Label {
    name: Option<String>,
    kind: LabelKind,
    stmt_start: u32,
}

/// Kinds of private name declarations, for the getter/setter pairing rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivateKind {
    Other,
    Getter { is_static: bool },
    Setter { is_static: bool },
    Pair,
}

#[derive(Default)]
pub(crate) struct PrivateScope {
    declared: HashMap<String, PrivateKind>,
    used: Vec<(String, Span)>,
}

pub(crate) struct Parser<'a> {
    pub(crate) lexer: Lexer<'a>,
    pub(crate) src: &'a str,
    pub(crate) tok: Token,
    pub(crate) prev_end: u32,
    pub(crate) module: bool,
    pub(crate) strict: bool,
    pub(crate) scopes: Vec<Scope>,
    pub(crate) labels: Vec<Label>,
    /// Position of the first YieldExpression / AwaitExpression / `await`
    /// identifier since the innermost function or cover started.
    pub(crate) yield_pos: Option<u32>,
    pub(crate) await_pos: Option<u32>,
    pub(crate) await_ident_pos: Option<u32>,
    /// Start of the AssignmentExpression that may be an arrow function.
    pub(crate) potential_arrow_at: Option<u32>,
    pub(crate) private: Vec<PrivateScope>,
    pub(crate) exported: HashSet<String>,
    pub(crate) undefined_exports: HashMap<String, Span>,
    pub(crate) options: ParseOptions,
    pub(crate) eval: EvalContext,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(
        src: &'a str,
        goal: Goal,
        options: &ParseOptions,
        allow_hashbang: bool,
    ) -> R<Parser<'a>> {
        let module = goal == Goal::Module;
        let mut lexer = Lexer::new(src, module, allow_hashbang);
        let tok = lexer.next_token()?;
        Ok(Parser {
            lexer,
            src,
            tok,
            prev_end: 0,
            module,
            strict: options.strict || module,
            scopes: vec![Scope::new(TOP)],
            labels: Vec::new(),
            yield_pos: None,
            await_pos: None,
            await_ident_pos: None,
            potential_arrow_at: None,
            private: Vec::new(),
            exported: HashSet::new(),
            undefined_exports: HashMap::new(),
            eval: options.eval.clone().unwrap_or_default(),
            options: options.clone(),
        })
    }

    fn parse_program(mut self) -> R<Program> {
        if !self.strict && self.prologue_has_use_strict() {
            self.strict = true;
        }
        let mut body = Vec::new();
        self.parse_directives(&mut body)?;
        while !self.is(T::Eof) {
            body.push(self.parse_statement_list_item()?);
        }
        if let Some((name, span)) = self
            .undefined_exports
            .iter()
            .min_by_key(|(_, span)| span.start)
            .map(|(n, s)| (n.clone(), *s))
        {
            return Err(self.error(span, &format!("export '{name}' is not defined")));
        }
        let scope = self.scopes.pop().unwrap();
        Ok(Program {
            span: Span::new(0, self.src.len() as u32),
            goal: if self.module {
                Goal::Module
            } else {
                Goal::Script
            },
            strict: self.strict,
            body,
            scope: scope.info,
        })
    }

    // ----- Tokens -----

    pub(crate) fn next(&mut self) -> R<()> {
        self.prev_end = self.tok.span.end;
        self.tok = self.lexer.next_token()?;
        Ok(())
    }

    pub(crate) fn start(&self) -> u32 {
        self.tok.span.start
    }

    pub(crate) fn span_from(&self, start: u32) -> Span {
        Span::new(start, self.prev_end.max(start))
    }

    pub(crate) fn is(&self, kind: T) -> bool {
        self.tok.kind == kind
    }

    pub(crate) fn eat(&mut self, kind: T) -> R<bool> {
        if self.is(kind) {
            self.next()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn expect(&mut self, kind: T) -> R<()> {
        if self.eat(kind)? {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    /// The current token is the keyword or contextual keyword `kw`, written
    /// without escapes.
    pub(crate) fn is_kw(&self, kw: &str) -> bool {
        self.tok.kind == T::Name && !self.tok.escaped && self.tok.name() == kw
    }

    pub(crate) fn eat_kw(&mut self, kw: &str) -> R<bool> {
        if self.is_kw(kw) {
            self.next()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn expect_kw(&mut self, kw: &str) -> R<()> {
        if self.eat_kw(kw)? {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    /// The token after the current one (`None` if it does not lex).
    pub(crate) fn peek(&self) -> Option<Token> {
        self.lexer.clone().next_token().ok()
    }

    /// The two tokens after the current one.
    pub(crate) fn peek2(&self) -> Option<(Token, Token)> {
        let mut lexer = self.lexer.clone();
        let first = lexer.next_token().ok()?;
        let second = lexer.next_token().ok()?;
        Some((first, second))
    }

    pub(crate) fn can_insert_semicolon(&self) -> bool {
        self.is(T::Eof) || self.is(T::RBrace) || self.tok.nl_before
    }

    /// Consumes `;` or applies automatic semicolon insertion.
    pub(crate) fn semicolon(&mut self) -> R<()> {
        if !self.eat(T::Semi)? && !self.can_insert_semicolon() {
            return Err(self.unexpected());
        }
        Ok(())
    }

    pub(crate) fn error(&self, span: Span, message: &str) -> SyntaxError {
        let (line, column) = line_column(self.src, span.start);
        SyntaxError {
            message: message.to_string(),
            span,
            line,
            column,
        }
    }

    pub(crate) fn error_at(&self, pos: u32, message: &str) -> SyntaxError {
        self.error(Span::new(pos, pos), message)
    }

    pub(crate) fn unexpected(&self) -> SyntaxError {
        if self.is(T::Eof) {
            self.error(self.tok.span, "unexpected end of input")
        } else {
            let text = self.tok.span.text(self.src);
            self.error(self.tok.span, &format!("unexpected token '{text}'"))
        }
    }

    /// Looks ahead from the current token over a directive prologue and
    /// reports whether it holds a Use Strict Directive. Run before parsing a
    /// body so that its parameters and every token are checked as strict.
    pub(crate) fn prologue_has_use_strict(&self) -> bool {
        let mut lexer = self.lexer.clone();
        let mut tok = self.tok.clone();
        loop {
            if tok.kind != T::String {
                return false;
            }
            let raw = tok.span.text(self.src);
            let Ok(next) = lexer.next_token() else {
                return false;
            };
            let ends = matches!(next.kind, T::Semi | T::RBrace | T::Eof)
                || (next.nl_before && !continues_expression(&next));
            if !ends {
                return false;
            }
            if raw == "'use strict'" || raw == "\"use strict\"" {
                return true;
            }
            tok = if next.kind == T::Semi {
                match lexer.next_token() {
                    Ok(t) => t,
                    Err(_) => return false,
                }
            } else {
                next
            };
        }
    }

    /// Parses the directive prologue at the current position (the caller has
    /// already set strict mode from [`Self::prologue_has_use_strict`]).
    pub(crate) fn parse_directives(&mut self, body: &mut Vec<Stmt>) -> R<()> {
        use crate::ast::{ExprKind, StmtKind};
        while self.is(T::String) {
            let tok_span = self.tok.span;
            let stmt = self.parse_statement_list_item()?;
            let directive = match &stmt.kind {
                StmtKind::Expr(e) if e.span == tok_span => match &e.kind {
                    ExprKind::String(value) => Some(value.clone()),
                    _ => None,
                },
                _ => None,
            };
            match directive {
                Some(value) => body.push(Stmt {
                    span: stmt.span,
                    kind: StmtKind::Directive {
                        value,
                        raw: tok_span.text(self.src).to_string(),
                    },
                }),
                None => {
                    body.push(stmt);
                    break;
                }
            }
        }
        Ok(())
    }

    // ----- Scopes -----

    pub(crate) fn enter_scope(&mut self, flags: u32) {
        self.scopes.push(Scope::new(flags));
    }

    pub(crate) fn exit_scope(&mut self) -> ScopeInfo {
        self.scopes.pop().map(|s| s.info).unwrap_or_default()
    }

    pub(crate) fn scope_flags(&self) -> u32 {
        self.scopes.last().map_or(0, |s| s.flags)
    }

    fn treats_functions_as_var(&self, scope: &Scope) -> bool {
        scope.flags & (FUNCTION | STATIC_BLOCK) != 0 || (!self.module && scope.flags & TOP != 0)
    }

    /// Functions declared in the current scope are var-scoped.
    pub(crate) fn functions_as_var(&self) -> bool {
        self.treats_functions_as_var(self.scopes.last().unwrap())
    }

    pub(crate) fn declare_name(&mut self, name: &str, bind: Bind, span: Span) -> R<()> {
        let n = self.scopes.len();
        let redeclared = match bind {
            Bind::Lexical => {
                let scope = &self.scopes[n - 1];
                let redeclared = scope.lexical.contains(name)
                    || scope.functions.contains(name)
                    || scope.var.contains(name);
                let is_var_scope = scope.flags & VAR_SCOPE != 0;
                let is_top = scope.flags & TOP != 0;
                let scope = &mut self.scopes[n - 1];
                scope.lexical.insert(name.to_string());
                if is_var_scope {
                    scope.info.lexical_names.push(name.to_string());
                }
                if self.module && is_top {
                    self.undefined_exports.remove(name);
                }
                redeclared
            }
            Bind::SimpleCatch => {
                let scope = &mut self.scopes[n - 1];
                scope.lexical.insert(name.to_string());
                scope.catch_param = Some(name.to_string());
                false
            }
            Bind::Function => {
                let as_var = self.treats_functions_as_var(&self.scopes[n - 1]);
                let scope = &self.scopes[n - 1];
                let redeclared = if as_var {
                    scope.lexical.contains(name)
                } else {
                    scope.lexical.contains(name) || scope.var.contains(name)
                };
                let is_var_scope = scope.flags & VAR_SCOPE != 0;
                let scope = &mut self.scopes[n - 1];
                scope.functions.insert(name.to_string());
                if is_var_scope && as_var && !scope.info.var_names.iter().any(|v| v == name) {
                    scope.info.var_names.push(name.to_string());
                }
                redeclared
            }
            Bind::Var | Bind::Param => {
                let mut redeclared = false;
                for i in (0..n).rev() {
                    let as_var = self.treats_functions_as_var(&self.scopes[i]);
                    let scope = &self.scopes[i];
                    let is_catch_param = scope.flags & SIMPLE_CATCH != 0
                        && scope.catch_param.as_deref() == Some(name);
                    if (scope.lexical.contains(name) && !is_catch_param)
                        || (!as_var && scope.functions.contains(name))
                    {
                        redeclared = true;
                        break;
                    }
                    let flags = scope.flags;
                    let scope = &mut self.scopes[i];
                    scope.var.insert(name.to_string());
                    if self.module && flags & TOP != 0 {
                        self.undefined_exports.remove(name);
                    }
                    if flags & VAR_SCOPE != 0 {
                        let scope = &mut self.scopes[i];
                        if bind == Bind::Var && !scope.info.var_names.iter().any(|v| v == name) {
                            scope.info.var_names.push(name.to_string());
                        }
                        break;
                    }
                }
                redeclared
            }
        };
        if redeclared {
            return Err(self.error(
                span,
                &format!("identifier '{name}' has already been declared"),
            ));
        }
        Ok(())
    }

    /// Flags of the nearest scope matching `mask`, walking outwards.
    fn find_scope(&self, mask: u32) -> u32 {
        self.scopes
            .iter()
            .rev()
            .find(|s| s.flags & mask != 0)
            .map_or(TOP, |s| s.flags)
    }

    /// Flags of the nearest var scope that is not an arrow function: the
    /// scope that provides `this`, `super`, `arguments` and `new.target`.
    pub(crate) fn this_scope_flags(&self) -> u32 {
        self.scopes
            .iter()
            .rev()
            .find(|s| s.flags & VAR_SCOPE != 0 && s.flags & ARROW == 0)
            .map_or(TOP, |s| s.flags)
    }

    /// `yield` is a keyword: inside a generator body or its parameters.
    pub(crate) fn in_generator(&self) -> bool {
        let flags = self.find_scope(FUNCTION | STATIC_BLOCK | FIELD_INIT);
        flags & FUNCTION != 0 && flags & GENERATOR != 0
    }

    /// An AwaitExpression may appear here.
    pub(crate) fn can_await(&self) -> bool {
        let flags = self.find_scope(FUNCTION | STATIC_BLOCK | FIELD_INIT);
        if flags & FUNCTION != 0 {
            flags & ASYNC != 0
        } else if flags & (STATIC_BLOCK | FIELD_INIT) != 0 {
            false
        } else {
            self.module
        }
    }

    /// `await` cannot be an identifier here: modules, async functions and
    /// class static blocks (field initializers inherit the enclosing [Await]).
    pub(crate) fn await_reserved(&self) -> bool {
        if self.module {
            return true;
        }
        let flags = self.find_scope(FUNCTION | STATIC_BLOCK);
        if flags & FUNCTION != 0 {
            flags & ASYNC != 0
        } else {
            flags & STATIC_BLOCK != 0
        }
    }

    pub(crate) fn in_function(&self) -> bool {
        self.find_scope(VAR_SCOPE) & FUNCTION != 0
    }

    pub(crate) fn allow_new_target(&self) -> bool {
        let flags = self.this_scope_flags();
        if flags & (FUNCTION | STATIC_BLOCK | FIELD_INIT) != 0 {
            true
        } else {
            self.eval.new_target
        }
    }

    pub(crate) fn allow_super_property(&self) -> bool {
        let flags = self.this_scope_flags();
        if flags & TOP != 0 {
            self.eval.super_property
        } else {
            flags & SUPER != 0
        }
    }

    pub(crate) fn allow_super_call(&self) -> bool {
        let flags = self.this_scope_flags();
        if flags & TOP != 0 {
            self.eval.super_call
        } else {
            flags & DIRECT_SUPER != 0
        }
    }

    fn arguments_forbidden(&self) -> bool {
        let flags = self.this_scope_flags();
        if flags & TOP != 0 {
            self.eval.class_field_initializer
        } else {
            flags & (FIELD_INIT | STATIC_BLOCK) != 0
        }
    }

    fn mark_this_scopes(&mut self, mark: fn(&mut ScopeInfo)) {
        for scope in self.scopes.iter_mut().rev() {
            if scope.flags & VAR_SCOPE != 0 {
                mark(&mut scope.info);
                if scope.flags & ARROW == 0 {
                    break;
                }
            }
        }
    }

    pub(crate) fn note_this(&mut self) {
        self.mark_this_scopes(|info| info.uses_this = true);
    }

    pub(crate) fn note_arguments(&mut self) {
        self.mark_this_scopes(|info| info.uses_arguments = true);
    }

    pub(crate) fn note_direct_eval(&mut self) {
        for scope in self.scopes.iter_mut() {
            if scope.flags & VAR_SCOPE != 0 {
                scope.info.has_direct_eval = true;
            }
        }
    }

    // ----- Identifiers -----

    /// Early errors of IdentifierReference, BindingIdentifier and
    /// LabelIdentifier (ECMA-262 §13.1.1) for the StringValue `name`.
    pub(crate) fn check_identifier(&mut self, name: &str, span: Span, usage: Usage) -> R<()> {
        if KEYWORDS.contains(&name) {
            return Err(self.error(span, &format!("unexpected keyword '{name}'")));
        }
        if self.strict && STRICT_RESERVED.contains(&name) {
            return Err(self.error(span, &format!("'{name}' is a reserved word in strict mode")));
        }
        if name == "yield" && self.in_generator() {
            return Err(self.error(span, "'yield' cannot be an identifier in a generator"));
        }
        if name == "await" {
            if self.await_reserved() {
                return Err(self.error(span, "'await' cannot be an identifier here"));
            }
            if self.await_ident_pos.is_none() {
                self.await_ident_pos = Some(span.start);
            }
        }
        if usage == Usage::Binding && self.strict && (name == "eval" || name == "arguments") {
            return Err(self.error(span, &format!("cannot bind '{name}' in strict mode")));
        }
        if name == "arguments" && usage != Usage::Label && self.arguments_forbidden() {
            return Err(self.error(
                span,
                "'arguments' is not allowed in class field initializers and static blocks",
            ));
        }
        Ok(())
    }

    /// Parses an IdentifierName (reserved words included).
    pub(crate) fn parse_identifier_name(&mut self) -> R<(String, Span)> {
        if !self.is(T::Name) {
            return Err(self.unexpected());
        }
        let name = self.tok.name().to_string();
        let span = self.tok.span;
        self.next()?;
        Ok((name, span))
    }

    /// Parses an Identifier in the given role, applying its early errors.
    pub(crate) fn parse_ident(&mut self, usage: Usage) -> R<(String, Span)> {
        let (name, span) = self.parse_identifier_name()?;
        self.check_identifier(&name, span, usage)?;
        Ok((name, span))
    }

    // ----- Private names -----

    pub(crate) fn enter_class_body(&mut self) {
        self.private.push(PrivateScope::default());
    }

    pub(crate) fn declare_private(&mut self, name: &str, kind: PrivateKind, span: Span) -> R<()> {
        let existing = self.private.last().unwrap().declared.get(name).copied();
        let next = match (existing, kind) {
            (None, kind) => kind,
            (Some(PrivateKind::Getter { is_static: a }), PrivateKind::Setter { is_static: b })
            | (Some(PrivateKind::Setter { is_static: a }), PrivateKind::Getter { is_static: b })
                if a == b =>
            {
                PrivateKind::Pair
            }
            _ => {
                return Err(self.error(
                    span,
                    &format!("private name '#{name}' has already been declared"),
                ));
            }
        };
        let scope = self.private.last_mut().unwrap();
        scope.declared.insert(name.to_string(), next);
        Ok(())
    }

    pub(crate) fn use_private(&mut self, name: &str, span: Span) -> R<()> {
        if let Some(scope) = self.private.last_mut() {
            scope.used.push((name.to_string(), span));
            Ok(())
        } else if self.eval.private_names.iter().any(|n| n == name) {
            Ok(())
        } else {
            Err(self.error(span, &format!("private name '#{name}' is not defined")))
        }
    }

    pub(crate) fn exit_class_body(&mut self) -> R<()> {
        let scope = self.private.pop().unwrap();
        for (name, span) in scope.used {
            if scope.declared.contains_key(&name) {
                continue;
            }
            if let Some(parent) = self.private.last_mut() {
                parent.used.push((name, span));
            } else if !self.eval.private_names.iter().any(|n| *n == name) {
                return Err(self.error(span, &format!("private name '#{name}' is not defined")));
            }
        }
        Ok(())
    }

    // ----- Cover grammar errors -----

    /// Raises the errors of a cover that stays an expression.
    pub(crate) fn check_expression_errors(&self, cover: &Cover) -> R<()> {
        if let Some(pos) = cover.shorthand_assign {
            return Err(self.error_at(
                pos,
                "shorthand property initializers are only valid in patterns",
            ));
        }
        if let Some(pos) = cover.double_proto {
            return Err(self.error_at(pos, "duplicate __proto__ property"));
        }
        Ok(())
    }

    pub(crate) fn has_expression_errors(cover: &Cover) -> bool {
        cover.shorthand_assign.is_some() || cover.double_proto.is_some()
    }

    /// Raises the errors of a cover that becomes a pattern.
    pub(crate) fn check_pattern_errors(&self, cover: &Cover, is_assign: bool) -> R<()> {
        if let Some(pos) = cover.trailing_comma {
            return Err(self.error_at(pos, "a rest element cannot be followed by a comma"));
        }
        let paren = if is_assign {
            cover.paren_assign
        } else {
            cover.paren_bind
        };
        if let Some(pos) = paren {
            return Err(self.error_at(pos, "invalid parenthesized pattern"));
        }
        Ok(())
    }

    pub(crate) fn check_yield_await_in_params(&self) -> R<()> {
        if let Some(pos) = self.yield_pos {
            if self.await_pos.is_none_or(|a| pos < a) {
                return Err(self.error_at(pos, "yield expression in formal parameters"));
            }
        }
        if let Some(pos) = self.await_pos {
            return Err(self.error_at(pos, "await expression in formal parameters"));
        }
        Ok(())
    }

    pub(crate) fn add_export_name(&mut self, name: &str, span: Span) -> R<()> {
        if !self.exported.insert(name.to_string()) {
            return Err(self.error(span, &format!("duplicate export '{name}'")));
        }
        Ok(())
    }

    /// Records a local name exported by `export { name }`; it must be
    /// declared at the module top level by the end of the module.
    pub(crate) fn check_local_export(&mut self, name: &str, span: Span) {
        let top = &self.scopes[0];
        if !top.lexical.contains(name) && !top.var.contains(name) {
            self.undefined_exports
                .entry(name.to_string())
                .or_insert(span);
        }
    }
}

/// ReservedWord minus `await`, `yield` and the literals handled elsewhere.
pub(crate) const KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
];

/// Identifiers reserved only in strict mode code.
pub(crate) const STRICT_RESERVED: &[&str] = &[
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "yield",
];

/// A token after a line break that continues the preceding expression (so
/// no semicolon is inserted).
pub(crate) fn continues_expression(tok: &Token) -> bool {
    match tok.kind {
        T::Name => !tok.escaped && matches!(tok.name(), "in" | "instanceof"),
        T::LParen
        | T::LBracket
        | T::Dot
        | T::QuestionDot
        | T::Template
        | T::Comma
        | T::Question
        | T::Colon
        | T::Arrow
        | T::Plus
        | T::Minus
        | T::Star
        | T::Slash
        | T::Percent
        | T::StarStar
        | T::Lt
        | T::Gt
        | T::LtEq
        | T::GtEq
        | T::Eq
        | T::NotEq
        | T::StrictEq
        | T::StrictNotEq
        | T::Shl
        | T::Shr
        | T::UShr
        | T::Amp
        | T::Pipe
        | T::Caret
        | T::AndAnd
        | T::OrOr
        | T::Nullish
        | T::Assign
        | T::PlusAssign
        | T::MinusAssign
        | T::StarAssign
        | T::SlashAssign
        | T::PercentAssign
        | T::StarStarAssign
        | T::ShlAssign
        | T::ShrAssign
        | T::UShrAssign
        | T::AmpAssign
        | T::PipeAssign
        | T::CaretAssign
        | T::AndAndAssign
        | T::OrOrAssign
        | T::NullishAssign => true,
        _ => false,
    }
}

/// The StringValue of a string token.
pub(crate) fn string_value(tok: &Token) -> Vec<u16> {
    match &tok.value {
        Value::String(s) => s.clone(),
        _ => Vec::new(),
    }
}
