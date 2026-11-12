//! Functions, arrow functions, methods, binding patterns and classes
//! (ECMA-262 §14.3.3, §15).

use super::{
    ARROW, ASYNC, Bind, DIRECT_SUPER, FIELD_INIT, FUNCTION, GENERATOR, Parser, PrivateKind, R,
    STATIC_BLOCK, STRICT_RESERVED, SUPER, Usage,
};
use crate::ast::{
    Class, ClassElement, ClassElementKind, Expr, ExprKind, Function, FunctionBody, FunctionKind,
    MethodKind, Name, Params, Pattern, PatternElement, PatternKind, PatternProperty, PropertyKey,
    Span, Stmt,
};
use crate::lexer::T;

/// How a `function` is being parsed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FnSite {
    Expression,
    /// A declaration; `hanging` for the Annex B `if (x) function f() {}`
    /// (not bound in the enclosing scope); `default_export` allows a
    /// missing name.
    Declaration {
        hanging: bool,
        default_export: bool,
    },
}

/// Appends the BoundNames of a pattern.
pub(crate) fn bound_names(p: &Pattern, out: &mut Vec<(String, Span)>) {
    match &p.kind {
        PatternKind::Ident(name) => out.push((name.clone(), p.span)),
        PatternKind::Expr(_) => {}
        PatternKind::Array { elements, rest } => {
            for element in elements.iter().flatten() {
                bound_names(&element.target, out);
            }
            if let Some(rest) = rest {
                bound_names(rest, out);
            }
        }
        PatternKind::Object { properties, rest } => {
            for property in properties {
                bound_names(&property.value.target, out);
            }
            if let Some(rest) = rest {
                bound_names(rest, out);
            }
        }
    }
}

fn params_bound_names(params: &Params) -> Vec<(String, Span)> {
    let mut names = Vec::new();
    for item in &params.items {
        bound_names(&item.target, &mut names);
    }
    if let Some(rest) = &params.rest {
        bound_names(rest, &mut names);
    }
    names
}

fn is_simple_params(items: &[PatternElement], rest: &Option<Box<Pattern>>) -> bool {
    rest.is_none()
        && items
            .iter()
            .all(|e| e.default.is_none() && matches!(e.target.kind, PatternKind::Ident(_)))
}

fn key_is(key: &PropertyKey, name: &str) -> bool {
    match key {
        PropertyKey::Name(n) => n == name,
        PropertyKey::String(s) => *s == name.encode_utf16().collect::<Vec<_>>(),
        _ => false,
    }
}

impl<'a> Parser<'a> {
    /// Declares every name bound by a pattern.
    pub(crate) fn declare_pattern(&mut self, p: &Pattern, bind: Bind) -> R<()> {
        let mut names = Vec::new();
        bound_names(p, &mut names);
        for (name, span) in names {
            if bind == Bind::Lexical && name == "let" {
                return Err(self.error(span, "'let' cannot be a lexically bound name"));
            }
            self.declare_name(&name, bind, span)?;
        }
        Ok(())
    }

    /// BindingIdentifier or BindingPattern (names are not declared here).
    pub(crate) fn parse_binding_target(&mut self) -> R<Pattern> {
        match self.tok.kind {
            T::LBracket => self.parse_array_binding(),
            T::LBrace => self.parse_object_binding(),
            T::Name => {
                let (name, span) = self.parse_ident(Usage::Binding)?;
                Ok(Pattern {
                    span,
                    kind: PatternKind::Ident(name),
                })
            }
            _ => Err(self.unexpected()),
        }
    }

    /// BindingElement: a target with an optional initializer.
    pub(crate) fn parse_binding_element(&mut self) -> R<PatternElement> {
        let start = self.start();
        let target = self.parse_binding_target()?;
        let default = if self.eat(T::Assign)? {
            Some(Box::new(self.parse_assign(false)?))
        } else {
            None
        };
        Ok(PatternElement {
            span: self.span_from(start),
            target,
            default,
        })
    }

    fn parse_array_binding(&mut self) -> R<Pattern> {
        let start = self.start();
        self.next()?;
        let mut elements = Vec::new();
        let mut rest = None;
        loop {
            if self.eat(T::RBracket)? {
                break;
            }
            if self.eat(T::Comma)? {
                elements.push(None);
                continue;
            }
            if self.eat(T::Ellipsis)? {
                rest = Some(Box::new(self.parse_binding_target()?));
                if self.is(T::Comma) {
                    return Err(self.error(self.tok.span, "a rest element must be last"));
                }
                self.expect(T::RBracket)?;
                break;
            }
            elements.push(Some(self.parse_binding_element()?));
            if !self.is(T::RBracket) {
                self.expect(T::Comma)?;
            }
        }
        Ok(Pattern {
            span: self.span_from(start),
            kind: PatternKind::Array { elements, rest },
        })
    }

    fn parse_object_binding(&mut self) -> R<Pattern> {
        let start = self.start();
        self.next()?;
        let mut properties = Vec::new();
        let mut rest = None;
        loop {
            if self.eat(T::RBrace)? {
                break;
            }
            if self.eat(T::Ellipsis)? {
                let (name, span) = self.parse_ident(Usage::Binding)?;
                rest = Some(Box::new(Pattern {
                    span,
                    kind: PatternKind::Ident(name),
                }));
                if self.is(T::Comma) {
                    return Err(self.error(self.tok.span, "a rest element must be last"));
                }
                self.expect(T::RBrace)?;
                break;
            }
            let prop_start = self.start();
            let key_tok = self.tok.clone();
            let key = self.parse_property_key()?;
            let (value, shorthand) = if self.eat(T::Colon)? {
                (self.parse_binding_element()?, false)
            } else {
                let PropertyKey::Name(name) = &key else {
                    return Err(self.unexpected());
                };
                if key_tok.kind != T::Name {
                    return Err(self.unexpected());
                }
                self.check_identifier(name, key_tok.span, Usage::Binding)?;
                let target = Pattern {
                    span: key_tok.span,
                    kind: PatternKind::Ident(name.clone()),
                };
                let default = if self.eat(T::Assign)? {
                    Some(Box::new(self.parse_assign(false)?))
                } else {
                    None
                };
                (
                    PatternElement {
                        span: self.span_from(prop_start),
                        target,
                        default,
                    },
                    true,
                )
            };
            properties.push(PatternProperty {
                span: self.span_from(prop_start),
                key,
                value,
                shorthand,
            });
            if !self.is(T::RBrace) {
                self.expect(T::Comma)?;
            }
        }
        Ok(Pattern {
            span: self.span_from(start),
            kind: PatternKind::Object { properties, rest },
        })
    }

    /// FormalParameters in parentheses.
    pub(crate) fn parse_formal_params(&mut self) -> R<Params> {
        let start = self.start();
        self.expect(T::LParen)?;
        let mut items = Vec::new();
        let mut rest = None;
        let mut first = true;
        while !self.eat(T::RParen)? {
            if !first {
                self.expect(T::Comma)?;
                if self.eat(T::RParen)? {
                    break;
                }
            }
            first = false;
            if self.eat(T::Ellipsis)? {
                rest = Some(Box::new(self.parse_binding_target()?));
                if self.is(T::Comma) {
                    return Err(self.error(self.tok.span, "a rest parameter must be last"));
                }
                self.expect(T::RParen)?;
                break;
            }
            items.push(self.parse_binding_element()?);
        }
        let is_simple = is_simple_params(&items, &rest);
        Ok(Params {
            span: self.span_from(start),
            items,
            rest,
            is_simple,
        })
    }

    /// Declares the parameters in the current (function) scope with the
    /// early errors that depend on the final strictness of the function:
    /// duplicates (allowed only in sloppy, simple, non-arrow, non-method
    /// lists) and reserved names in strict mode.
    fn declare_params(&mut self, params: &Params, allow_duplicates: bool) -> R<()> {
        let mut seen: Vec<&str> = Vec::new();
        let names = params_bound_names(params);
        for (name, span) in &names {
            if self.strict {
                self.check_strict_binding_name(name, *span)?;
            }
            if seen.contains(&name.as_str()) {
                if !allow_duplicates {
                    return Err(self.error(*span, &format!("duplicate parameter name '{name}'")));
                }
            } else {
                seen.push(name);
            }
            self.declare_name(name, Bind::Param, *span)?;
        }
        Ok(())
    }

    /// Strict mode errors of a BindingIdentifier checked after the fact
    /// (parameters and names of functions whose body is strict).
    fn check_strict_binding_name(&self, name: &str, span: Span) -> R<()> {
        if name == "eval" || name == "arguments" {
            return Err(self.error(span, &format!("cannot bind '{name}' in strict mode")));
        }
        if STRICT_RESERVED.contains(&name) {
            return Err(self.error(span, &format!("'{name}' is a reserved word in strict mode")));
        }
        Ok(())
    }

    /// `{ FunctionBody }` of a non-arrow function or of an arrow with a
    /// block body; the function scope is current and holds no names yet.
    /// Returns the statements and whether the function is strict.
    fn parse_function_body(
        &mut self,
        params: &Params,
        name: Option<&Name>,
        allow_duplicates: bool,
    ) -> R<(Vec<Stmt>, bool, bool)> {
        self.expect(T::LBrace)?;
        let old_strict = self.strict;
        let use_strict = self.prologue_has_use_strict();
        if use_strict && !params.is_simple {
            return Err(self.error(
                params.span,
                "'use strict' is not allowed in functions with non-simple parameters",
            ));
        }
        if use_strict {
            self.strict = true;
        }
        let old_labels = std::mem::take(&mut self.labels);
        self.declare_params(params, allow_duplicates && !self.strict && params.is_simple)?;
        if self.strict && !old_strict {
            if let Some(name) = name {
                self.check_strict_binding_name(&name.name, name.span)?;
            }
        }
        let mut body = Vec::new();
        self.parse_directives(&mut body)?;
        while !self.is(T::RBrace) {
            body.push(self.parse_statement_list_item()?);
        }
        let strict = self.strict;
        self.strict = old_strict;
        self.labels = old_labels;
        self.next()?;
        Ok((body, strict, use_strict))
    }

    fn function_flags(is_async: bool, is_generator: bool) -> u32 {
        let mut flags = FUNCTION;
        if is_async {
            flags |= ASYNC;
        }
        if is_generator {
            flags |= GENERATOR;
        }
        flags
    }

    /// `function` declarations and expressions; the current token is
    /// `function` (after `async` for async functions). `start` is the
    /// start of the source text (the `async` keyword if present).
    pub(crate) fn parse_function(
        &mut self,
        start: u32,
        is_async: bool,
        site: FnSite,
    ) -> R<Function> {
        self.expect_kw("function")?;
        let is_generator = self.eat(T::Star)?;
        let mut name = None;
        if let FnSite::Declaration {
            hanging,
            default_export,
        } = site
        {
            if hanging && is_generator {
                return Err(self.error(
                    self.span_from(start),
                    "generator declarations are not allowed here",
                ));
            }
            if !(default_export && !self.is(T::Name)) {
                let (n, span) = self.parse_ident(Usage::Binding)?;
                if !hanging {
                    let bind = if self.strict || is_generator || is_async {
                        if self.functions_as_var() {
                            Bind::Var
                        } else {
                            Bind::Lexical
                        }
                    } else {
                        Bind::Function
                    };
                    self.declare_name(&n, bind, span)?;
                }
                name = Some(Name { span, name: n });
            }
        }
        let old_yield = self.yield_pos.take();
        let old_await = self.await_pos.take();
        let old_await_ident = self.await_ident_pos.take();
        self.enter_scope(Self::function_flags(is_async, is_generator));
        if site == FnSite::Expression && self.is(T::Name) {
            let (n, span) = self.parse_ident(Usage::Binding)?;
            name = Some(Name { span, name: n });
        }
        let params = self.parse_formal_params()?;
        self.check_yield_await_in_params()?;
        let (body, strict, use_strict) = self.parse_function_body(&params, name.as_ref(), true)?;
        let scope = self.exit_scope();
        self.yield_pos = old_yield;
        self.await_pos = old_await;
        self.await_ident_pos = old_await_ident;
        Ok(Function {
            span: self.span_from(start),
            name,
            kind: FunctionKind::Normal,
            is_async,
            is_generator,
            params,
            body: FunctionBody::Block(body),
            strict,
            use_strict_directive: use_strict,
            scope,
        })
    }

    /// A function expression; for async ones the current token is
    /// `function` and `async` started at `start`.
    pub(crate) fn parse_function_expression(&mut self, start: u32, is_async: bool) -> R<Function> {
        self.parse_function(start, is_async, FnSite::Expression)
    }

    /// The parameter list and body of a method (object literal or class);
    /// the current token is `(`. `start` begins the method's source text.
    pub(crate) fn parse_method(
        &mut self,
        start: u32,
        kind: MethodKind,
        is_async: bool,
        is_generator: bool,
        function_kind: FunctionKind,
        direct_super: bool,
    ) -> R<Function> {
        let old_yield = self.yield_pos.take();
        let old_await = self.await_pos.take();
        let old_await_ident = self.await_ident_pos.take();
        let mut flags = Self::function_flags(is_async, is_generator) | SUPER;
        if direct_super {
            flags |= DIRECT_SUPER;
        }
        self.enter_scope(flags);
        let params = self.parse_formal_params()?;
        self.check_yield_await_in_params()?;
        match kind {
            MethodKind::Get if !params.items.is_empty() || params.rest.is_some() => {
                return Err(self.error(params.span, "a getter must have no parameters"));
            }
            MethodKind::Set if params.items.len() != 1 || params.rest.is_some() => {
                return Err(self.error(params.span, "a setter must have exactly one parameter"));
            }
            _ => {}
        }
        let (body, strict, use_strict) = self.parse_function_body(&params, None, false)?;
        let scope = self.exit_scope();
        self.yield_pos = old_yield;
        self.await_pos = old_await;
        self.await_ident_pos = old_await_ident;
        Ok(Function {
            span: self.span_from(start),
            name: None,
            kind: function_kind,
            is_async,
            is_generator,
            params,
            body: FunctionBody::Block(body),
            strict,
            use_strict_directive: use_strict,
            scope,
        })
    }

    /// The body of an arrow function whose parameters (already converted
    /// from the cover) are given; the current token follows `=>`.
    pub(crate) fn parse_arrow(
        &mut self,
        start: u32,
        params_span: Span,
        items: Vec<PatternElement>,
        rest: Option<Box<Pattern>>,
        is_async: bool,
        no_in: bool,
    ) -> R<Expr> {
        let is_simple = is_simple_params(&items, &rest);
        let params = Params {
            span: params_span,
            items,
            rest,
            is_simple,
        };
        let old_yield = self.yield_pos.take();
        let old_await = self.await_pos.take();
        let old_await_ident = self.await_ident_pos.take();
        self.enter_scope(Self::function_flags(is_async, false) | ARROW);
        for (name, span) in params_bound_names(&params) {
            if is_async && name == "await" {
                return Err(self.error(
                    span,
                    "'await' cannot be a parameter of an async arrow function",
                ));
            }
            if name == "yield" && self.strict {
                return Err(self.error(span, "'yield' is a reserved word in strict mode"));
            }
        }
        let (body, strict, use_strict) = if self.is(T::LBrace) {
            let (body, strict, use_strict) = self.parse_function_body(&params, None, false)?;
            (FunctionBody::Block(body), strict, use_strict)
        } else {
            self.declare_params(&params, false)?;
            let old_labels = std::mem::take(&mut self.labels);
            let e = self.parse_assign(no_in)?;
            self.labels = old_labels;
            (FunctionBody::Expr(Box::new(e)), self.strict, false)
        };
        let scope = self.exit_scope();
        self.yield_pos = old_yield;
        self.await_pos = old_await;
        self.await_ident_pos = old_await_ident;
        let function = Function {
            span: self.span_from(start),
            name: None,
            kind: FunctionKind::Arrow,
            is_async,
            is_generator: false,
            params,
            body,
            strict,
            use_strict_directive: use_strict,
            scope,
        };
        Ok(Expr {
            span: function.span,
            kind: ExprKind::Arrow(Box::new(function)),
        })
    }

    // ----- Classes -----

    /// ClassDeclaration or ClassExpression; the current token is `class`.
    pub(crate) fn parse_class(
        &mut self,
        start: u32,
        is_decl: bool,
        default_export: bool,
    ) -> R<Class> {
        self.next()?;
        let old_strict = self.strict;
        self.strict = true;
        let name = if self.is(T::Name) && !self.is_kw("extends") {
            let (n, span) = self.parse_ident(Usage::Binding)?;
            if is_decl {
                if n == "let" {
                    return Err(self.error(span, "'let' cannot be a lexically bound name"));
                }
                self.declare_name(&n, Bind::Lexical, span)?;
            }
            Some(Name { span, name: n })
        } else {
            if is_decl && !default_export {
                return Err(self.unexpected());
            }
            None
        };
        let heritage = if self.eat_kw("extends")? {
            let heritage_start = self.start();
            let e = self.parse_subscripts_expr(None, false)?;
            if crate::parser::expr::is_arrow(&e) {
                return Err(self.error(
                    Span::new(heritage_start, self.prev_end),
                    "invalid class heritage",
                ));
            }
            Some(Box::new(e))
        } else {
            None
        };
        self.enter_class_body();
        self.expect(T::LBrace)?;
        let mut elements = Vec::new();
        let mut constructor = None;
        while !self.is(T::RBrace) {
            if self.eat(T::Semi)? {
                continue;
            }
            if let Some(element) = self.parse_class_element(heritage.is_some(), &mut constructor)? {
                elements.push(element);
            }
        }
        self.strict = old_strict;
        self.next()?;
        self.exit_class_body()?;
        Ok(Class {
            span: self.span_from(start),
            name,
            heritage,
            constructor,
            elements,
        })
    }

    fn is_class_element_name_start(&self) -> bool {
        matches!(
            self.tok.kind,
            T::Name | T::PrivateName | T::String | T::Number | T::BigInt | T::LBracket
        )
    }

    fn parse_class_element_name(&mut self) -> R<(PropertyKey, Span)> {
        let span = self.tok.span;
        if self.is(T::PrivateName) {
            let name = self.tok.name().to_string();
            if name == "constructor" {
                return Err(self.error(span, "'#constructor' is not a valid private name"));
            }
            self.next()?;
            return Ok((PropertyKey::Private(name), span));
        }
        let key = self.parse_property_key()?;
        Ok((key, self.span_from(span.start)))
    }

    /// One ClassElement (not `;`). The constructor is stored separately
    /// and reported as `None`.
    fn parse_class_element(
        &mut self,
        derived: bool,
        constructor: &mut Option<Box<Function>>,
    ) -> R<Option<ClassElement>> {
        let element_start = self.start();
        let mut is_static = false;
        let mut key_name: Option<(String, Span)> = None;
        if self.is_kw("static") {
            let span = self.tok.span;
            self.next()?;
            if self.is(T::LBrace) {
                return self.parse_static_block(element_start).map(Some);
            }
            if self.is_class_element_name_start() || self.is(T::Star) {
                is_static = true;
            } else {
                key_name = Some(("static".to_string(), span));
            }
        }
        let method_start = match &key_name {
            Some((_, span)) => span.start,
            None => self.start(),
        };
        let mut is_async = false;
        let mut is_generator = false;
        let mut kind = MethodKind::Method;
        if key_name.is_none() && self.is_kw("async") {
            let span = self.tok.span;
            self.next()?;
            if (self.is_class_element_name_start() || self.is(T::Star)) && !self.tok.nl_before {
                is_async = true;
            } else {
                key_name = Some(("async".to_string(), span));
            }
        }
        if key_name.is_none() && self.eat(T::Star)? {
            is_generator = true;
        }
        if key_name.is_none()
            && !is_async
            && !is_generator
            && (self.is_kw("get") || self.is_kw("set"))
        {
            let word = self.tok.name().to_string();
            let span = self.tok.span;
            self.next()?;
            if self.is_class_element_name_start() {
                kind = if word == "get" {
                    MethodKind::Get
                } else {
                    MethodKind::Set
                };
            } else {
                key_name = Some((word, span));
            }
        }
        let (key, key_span) = match key_name {
            Some((name, span)) => (PropertyKey::Name(name), span),
            None => self.parse_class_element_name()?,
        };
        let is_constructor = !is_static && key_is(&key, "constructor");
        if self.is(T::LParen) || kind != MethodKind::Method || is_generator || is_async {
            if is_constructor {
                if kind != MethodKind::Method {
                    return Err(self.error(key_span, "a class constructor cannot be an accessor"));
                }
                if is_generator || is_async {
                    return Err(self.error(
                        key_span,
                        "a class constructor cannot be a generator or async",
                    ));
                }
                if constructor.is_some() {
                    return Err(self.error(key_span, "a class may only have one constructor"));
                }
                let function_kind = if derived {
                    FunctionKind::DerivedConstructor
                } else {
                    FunctionKind::ClassConstructor
                };
                let function =
                    self.parse_method(method_start, kind, false, false, function_kind, derived)?;
                *constructor = Some(Box::new(function));
                return Ok(None);
            }
            if is_static && key_is(&key, "prototype") {
                return Err(self.error(key_span, "a static method cannot be named 'prototype'"));
            }
            if let PropertyKey::Private(name) = &key {
                let private_kind = match kind {
                    MethodKind::Get => PrivateKind::Getter { is_static },
                    MethodKind::Set => PrivateKind::Setter { is_static },
                    MethodKind::Method => PrivateKind::Other,
                };
                self.declare_private(name, private_kind, key_span)?;
            }
            let function = self.parse_method(
                method_start,
                kind,
                is_async,
                is_generator,
                FunctionKind::Method,
                false,
            )?;
            return Ok(Some(ClassElement {
                span: self.span_from(element_start),
                is_static,
                kind: ClassElementKind::Method {
                    key,
                    kind,
                    function: Box::new(function),
                },
            }));
        }
        if key_is(&key, "constructor") {
            return Err(self.error(key_span, "a class field cannot be named 'constructor'"));
        }
        if is_static && key_is(&key, "prototype") {
            return Err(self.error(key_span, "a static field cannot be named 'prototype'"));
        }
        if let PropertyKey::Private(name) = &key {
            self.declare_private(name, PrivateKind::Other, key_span)?;
        }
        self.enter_scope(FIELD_INIT | SUPER);
        let old_labels = std::mem::take(&mut self.labels);
        let value = if self.eat(T::Assign)? {
            Some(Box::new(self.parse_assign(false)?))
        } else {
            None
        };
        self.labels = old_labels;
        let scope = self.exit_scope();
        self.semicolon()?;
        Ok(Some(ClassElement {
            span: self.span_from(element_start),
            is_static,
            kind: ClassElementKind::Field { key, value, scope },
        }))
    }

    /// `static { ... }`; the current token is `{`.
    fn parse_static_block(&mut self, start: u32) -> R<ClassElement> {
        self.next()?;
        let old_labels = std::mem::take(&mut self.labels);
        self.enter_scope(STATIC_BLOCK | SUPER);
        let mut body = Vec::new();
        while !self.is(T::RBrace) {
            body.push(self.parse_statement_list_item()?);
        }
        self.next()?;
        let scope = self.exit_scope();
        self.labels = old_labels;
        Ok(ClassElement {
            span: self.span_from(start),
            is_static: true,
            kind: ClassElementKind::StaticBlock { body, scope },
        })
    }

    // ----- Function constructor -----

    /// The parameters string of CreateDynamicFunction, parsed on its own.
    pub(crate) fn check_dynamic_params(&mut self, flags: u32) -> R<()> {
        self.enter_scope(flags);
        let mut first = true;
        while !self.is(T::Eof) {
            if !first {
                self.expect(T::Comma)?;
                if self.is(T::Eof) {
                    break;
                }
            }
            first = false;
            if self.eat(T::Ellipsis)? {
                self.parse_binding_target()?;
                if !self.is(T::Eof) {
                    return Err(self.unexpected());
                }
                break;
            }
            self.parse_binding_element()?;
        }
        self.check_yield_await_in_params()
    }

    /// The body string of CreateDynamicFunction, parsed on its own.
    pub(crate) fn check_dynamic_body(&mut self, flags: u32) -> R<()> {
        self.enter_scope(flags);
        if self.prologue_has_use_strict() {
            self.strict = true;
        }
        let mut body = Vec::new();
        self.parse_directives(&mut body)?;
        while !self.is(T::Eof) {
            self.parse_statement_list_item()?;
        }
        Ok(())
    }

    /// The assembled source text of CreateDynamicFunction.
    pub(crate) fn parse_dynamic_source(mut self) -> R<Function> {
        let is_async = self.eat_kw("async")?;
        let mut function = self.parse_function(0, is_async, FnSite::Expression)?;
        if !self.is(T::Eof) {
            return Err(self.unexpected());
        }
        // The function is not bound to the name "anonymous" in its scope.
        function.name = None;
        Ok(function)
    }
}
