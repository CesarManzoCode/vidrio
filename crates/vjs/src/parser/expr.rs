//! Expressions (ECMA-262 §13), including the cover grammars and their
//! conversion to assignment and binding patterns.

use super::{Cover, Parser, R, Usage, string_value};
use crate::ast::{
    Argument, ArrayElement, AssignOp, BinaryOp, Expr, ExprKind, LogicalOp, MemberProperty, Name,
    Pattern, PatternElement, PatternKind, PatternProperty, Property, PropertyKey, PropertyKind,
    Span, Template, TemplateElement, UnaryOp, UpdateOp,
};
use crate::lexer::{T, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
enum BinOp {
    Binary(BinaryOp),
    Logical(LogicalOp),
}

fn assign_op(kind: T) -> Option<AssignOp> {
    Some(match kind {
        T::Assign => AssignOp::Assign,
        T::PlusAssign => AssignOp::Add,
        T::MinusAssign => AssignOp::Sub,
        T::StarAssign => AssignOp::Mul,
        T::SlashAssign => AssignOp::Div,
        T::PercentAssign => AssignOp::Rem,
        T::StarStarAssign => AssignOp::Exp,
        T::ShlAssign => AssignOp::Shl,
        T::ShrAssign => AssignOp::Shr,
        T::UShrAssign => AssignOp::UShr,
        T::AmpAssign => AssignOp::BitAnd,
        T::PipeAssign => AssignOp::BitOr,
        T::CaretAssign => AssignOp::BitXor,
        T::AndAndAssign => AssignOp::And,
        T::OrOrAssign => AssignOp::Or,
        T::NullishAssign => AssignOp::Coalesce,
        _ => return None,
    })
}

/// An unparenthesized arrow function: it cannot be an operand.
pub(crate) fn is_arrow(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Arrow(_))
}

/// IsSimpleAssignmentTarget for the cover checks: an identifier or a
/// property access, possibly parenthesized.
fn is_simple_assign_target(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Paren(inner) => is_simple_assign_target(inner),
        ExprKind::Ident(_) | ExprKind::Member { .. } | ExprKind::SuperMember(_) => true,
        _ => false,
    }
}

/// A call through this callee may be a direct eval: the identifier `eval`,
/// possibly parenthesized (still a Reference).
fn is_eval_reference(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Ident(name) => name == "eval",
        ExprKind::Paren(inner) => is_eval_reference(inner),
        _ => false,
    }
}

fn is_local_variable_access(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Ident(_) => true,
        ExprKind::Paren(inner) => is_local_variable_access(inner),
        _ => false,
    }
}

fn is_private_access(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Member {
            property: MemberProperty::Private(_),
            ..
        } => true,
        ExprKind::OptionalChain(inner) | ExprKind::Paren(inner) => is_private_access(inner),
        _ => false,
    }
}

fn key_is_proto(key: &PropertyKey) -> bool {
    match key {
        PropertyKey::Name(name) => name == "__proto__",
        PropertyKey::String(s) => *s == "__proto__".encode_utf16().collect::<Vec<_>>(),
        _ => false,
    }
}

impl<'a> Parser<'a> {
    fn expr(&self, start: u32, kind: ExprKind) -> Expr {
        Expr {
            span: self.span_from(start),
            kind,
        }
    }

    /// Expression: AssignmentExpression (`,` AssignmentExpression)*.
    pub(crate) fn parse_expression(&mut self, no_in: bool) -> R<Expr> {
        let mut cover = Cover::default();
        self.parse_expression_cover(no_in, &mut cover, true)
    }

    /// Expression with a cover owned by the caller (`own` false): its
    /// errors are left for the caller to raise or discard.
    pub(crate) fn parse_expression_cover(
        &mut self,
        no_in: bool,
        cover: &mut Cover,
        own: bool,
    ) -> R<Expr> {
        let start = self.start();
        let first = self.parse_assign_cover(no_in, cover, own)?;
        if self.is(T::Comma) {
            let mut list = vec![first];
            while self.eat(T::Comma)? {
                list.push(self.parse_assign_cover(no_in, cover, own)?);
            }
            return Ok(self.expr(start, ExprKind::Sequence(list)));
        }
        Ok(first)
    }

    pub(crate) fn parse_assign(&mut self, no_in: bool) -> R<Expr> {
        let mut cover = Cover::default();
        self.parse_assign_cover(no_in, &mut cover, true)
    }

    /// AssignmentExpression.
    pub(crate) fn parse_assign_cover(
        &mut self,
        no_in: bool,
        cover: &mut Cover,
        own: bool,
    ) -> R<Expr> {
        if self.is_kw("yield") && self.in_generator() {
            return self.parse_yield(no_in);
        }
        let old_paren_assign = cover.paren_assign;
        let old_trailing_comma = cover.trailing_comma;
        let old_double_proto = cover.double_proto;
        if !own {
            cover.paren_assign = None;
            cover.trailing_comma = None;
        }
        let start = self.start();
        if self.is(T::LParen) || self.is(T::Name) {
            self.potential_arrow_at = Some(start);
        }
        let left = self.parse_conditional(no_in, cover)?;
        if let Some(op) = assign_op(self.tok.kind) {
            let target = if op == AssignOp::Assign {
                self.to_assign_pattern(left, Some(&*cover))?
            } else {
                let logical = matches!(op, AssignOp::And | AssignOp::Or | AssignOp::Coalesce);
                self.to_simple_target(left, !logical)?
            };
            if !own {
                cover.paren_assign = None;
                cover.trailing_comma = None;
                cover.double_proto = None;
            }
            if cover.shorthand_assign.is_some_and(|p| p >= start) {
                cover.shorthand_assign = None;
            }
            self.next()?;
            let value = self.parse_assign(no_in)?;
            if old_double_proto.is_some() {
                cover.double_proto = old_double_proto;
            }
            return Ok(self.expr(
                start,
                ExprKind::Assign {
                    op,
                    target: Box::new(target),
                    value: Box::new(value),
                },
            ));
        } else if own {
            self.check_expression_errors(cover)?;
        }
        if old_paren_assign.is_some() {
            cover.paren_assign = old_paren_assign;
        }
        if old_trailing_comma.is_some() {
            cover.trailing_comma = old_trailing_comma;
        }
        Ok(left)
    }

    fn parse_conditional(&mut self, no_in: bool, cover: &mut Cover) -> R<Expr> {
        let start = self.start();
        let test = self.parse_binary(no_in, cover)?;
        if Self::has_expression_errors(cover) || is_arrow(&test) {
            return Ok(test);
        }
        if self.eat(T::Question)? {
            let consequent = self.parse_assign(false)?;
            self.expect(T::Colon)?;
            let alternate = self.parse_assign(no_in)?;
            return Ok(self.expr(
                start,
                ExprKind::Conditional {
                    test: Box::new(test),
                    consequent: Box::new(consequent),
                    alternate: Box::new(alternate),
                },
            ));
        }
        Ok(test)
    }

    fn parse_binary(&mut self, no_in: bool, cover: &mut Cover) -> R<Expr> {
        let start = self.start();
        let left = self.parse_binary_operand(no_in, Some(&mut *cover), -1)?;
        if Self::has_expression_errors(cover) || is_arrow(&left) {
            return Ok(left);
        }
        self.parse_binary_rest(left, start, -1, no_in)
    }

    /// An operand of a binary operator; `#x in obj` is accepted here when
    /// the operator context allows a RelationalExpression.
    fn parse_binary_operand(
        &mut self,
        no_in: bool,
        cover: Option<&mut Cover>,
        min_prec: i32,
    ) -> R<Expr> {
        if self.is(T::PrivateName) {
            let start = self.start();
            let span = self.tok.span;
            let name = self.tok.name().to_string();
            self.next()?;
            if no_in || !self.is_kw("in") || min_prec >= 7 {
                return Err(self.error(span, "unexpected private name"));
            }
            self.use_private(&name, span)?;
            self.next()?;
            let right_start = self.start();
            let right = self.parse_unary(None, false, false, no_in)?;
            let right = self.parse_binary_rest(right, right_start, 7, no_in)?;
            return Ok(self.expr(
                start,
                ExprKind::PrivateIn {
                    name: Name { span, name },
                    right: Box::new(right),
                },
            ));
        }
        self.parse_unary(cover, false, false, no_in)
    }

    fn binary_op(&self) -> Option<(i32, BinOp)> {
        use BinaryOp as B;
        let op = match self.tok.kind {
            T::Nullish => return Some((1, BinOp::Logical(LogicalOp::Coalesce))),
            T::OrOr => return Some((1, BinOp::Logical(LogicalOp::Or))),
            T::AndAnd => return Some((2, BinOp::Logical(LogicalOp::And))),
            T::Pipe => (3, B::BitOr),
            T::Caret => (4, B::BitXor),
            T::Amp => (5, B::BitAnd),
            T::Eq => (6, B::Eq),
            T::NotEq => (6, B::NotEq),
            T::StrictEq => (6, B::StrictEq),
            T::StrictNotEq => (6, B::StrictNotEq),
            T::Lt => (7, B::Lt),
            T::Gt => (7, B::Gt),
            T::LtEq => (7, B::LtEq),
            T::GtEq => (7, B::GtEq),
            T::Name if self.is_kw("in") => (7, B::In),
            T::Name if self.is_kw("instanceof") => (7, B::InstanceOf),
            T::Shl => (8, B::Shl),
            T::Shr => (8, B::Shr),
            T::UShr => (8, B::UShr),
            T::Plus => (9, B::Add),
            T::Minus => (9, B::Sub),
            T::Star => (10, B::Mul),
            T::Slash => (10, B::Div),
            T::Percent => (10, B::Rem),
            _ => return None,
        };
        Some((op.0, BinOp::Binary(op.1)))
    }

    /// Binary operators after `left` whose precedence is above `min_prec`
    /// (left-associative, so a loop rather than recursion on the left).
    fn parse_binary_rest(
        &mut self,
        left: Expr,
        left_start: u32,
        min_prec: i32,
        no_in: bool,
    ) -> R<Expr> {
        let mut left = left;
        loop {
            let Some((prec, op)) = self.binary_op() else {
                return Ok(left);
            };
            if no_in && op == BinOp::Binary(BinaryOp::In) {
                return Ok(left);
            }
            if prec <= min_prec {
                return Ok(left);
            }
            let logical = matches!(op, BinOp::Logical(LogicalOp::And | LogicalOp::Or));
            let coalesce = op == BinOp::Logical(LogicalOp::Coalesce);
            // `??` binds its right operand like `&&` so that mixing is detected.
            let right_prec = if coalesce { 2 } else { prec };
            self.next()?;
            let right_start = self.start();
            let right = self.parse_binary_operand(no_in, None, right_prec)?;
            let right = self.parse_binary_rest(right, right_start, right_prec, no_in)?;
            let kind = match op {
                BinOp::Binary(op) => ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                BinOp::Logical(op) => ExprKind::Logical {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
            left = self.expr(left_start, kind);
            if (logical && self.is(T::Nullish))
                || (coalesce && (self.is(T::OrOr) || self.is(T::AndAnd)))
            {
                return Err(self.error(
                    self.tok.span,
                    "'??' cannot be mixed with '||' or '&&' without parentheses",
                ));
            }
        }
    }

    /// UnaryExpression, UpdateExpression and ExponentiationExpression.
    pub(crate) fn parse_unary(
        &mut self,
        cover: Option<&mut Cover>,
        saw_unary: bool,
        inc_dec: bool,
        no_in: bool,
    ) -> R<Expr> {
        let start = self.start();
        let mut saw_unary = saw_unary;
        let prefix = match self.tok.kind {
            T::Plus => Some(Ok(UnaryOp::Plus)),
            T::Minus => Some(Ok(UnaryOp::Minus)),
            T::Bang => Some(Ok(UnaryOp::Not)),
            T::Tilde => Some(Ok(UnaryOp::BitNot)),
            T::Inc => Some(Err(UpdateOp::Increment)),
            T::Dec => Some(Err(UpdateOp::Decrement)),
            T::Name if self.is_kw("typeof") => Some(Ok(UnaryOp::Typeof)),
            T::Name if self.is_kw("void") => Some(Ok(UnaryOp::Void)),
            T::Name if self.is_kw("delete") => Some(Ok(UnaryOp::Delete)),
            _ => None,
        };
        let expr = if self.is_kw("await") && self.can_await() {
            saw_unary = true;
            self.parse_await(no_in)?
        } else if let Some(prefix) = prefix {
            self.next()?;
            let update = prefix.is_err();
            let argument = self.parse_unary(None, true, update, no_in)?;
            match prefix {
                Err(op) => {
                    self.check_simple_target(&argument, true)?;
                    self.expr(
                        start,
                        ExprKind::Update {
                            op,
                            prefix: true,
                            argument: Box::new(argument),
                        },
                    )
                }
                Ok(op) => {
                    if op == UnaryOp::Delete {
                        if self.strict && is_local_variable_access(&argument) {
                            return Err(self.error(
                                self.span_from(start),
                                "cannot delete an unqualified identifier in strict mode",
                            ));
                        }
                        if is_private_access(&argument) {
                            return Err(self
                                .error(self.span_from(start), "private fields cannot be deleted"));
                        }
                    }
                    saw_unary = true;
                    self.expr(
                        start,
                        ExprKind::Unary {
                            op,
                            argument: Box::new(argument),
                        },
                    )
                }
            }
        } else {
            let mut cover = cover;
            let mut expr = self.parse_subscripts_expr(cover.as_deref_mut(), no_in)?;
            if cover.is_some_and(|c| Self::has_expression_errors(c)) {
                return Ok(expr);
            }
            while (self.is(T::Inc) || self.is(T::Dec)) && !self.tok.nl_before {
                let op = if self.is(T::Inc) {
                    UpdateOp::Increment
                } else {
                    UpdateOp::Decrement
                };
                self.check_simple_target(&expr, true)?;
                self.next()?;
                expr = self.expr(
                    start,
                    ExprKind::Update {
                        op,
                        prefix: false,
                        argument: Box::new(expr),
                    },
                );
            }
            expr
        };
        if !inc_dec && self.is(T::StarStar) {
            if saw_unary {
                return Err(self.error(
                    self.tok.span,
                    "unary operator before '**' must be parenthesized",
                ));
            }
            self.next()?;
            let right = self.parse_unary(None, false, false, no_in)?;
            return Ok(self.expr(
                start,
                ExprKind::Binary {
                    op: BinaryOp::Exp,
                    left: Box::new(expr),
                    right: Box::new(right),
                },
            ));
        }
        Ok(expr)
    }

    fn parse_await(&mut self, no_in: bool) -> R<Expr> {
        let start = self.start();
        if self.await_pos.is_none() {
            self.await_pos = Some(start);
        }
        self.next()?;
        let argument = self.parse_unary(None, true, false, no_in)?;
        Ok(self.expr(start, ExprKind::Await(Box::new(argument))))
    }

    fn starts_expr(&self) -> bool {
        match self.tok.kind {
            T::Name => !(self.is_kw("in") || self.is_kw("instanceof")),
            T::Number
            | T::BigInt
            | T::String
            | T::Template
            | T::LBrace
            | T::LParen
            | T::LBracket
            | T::Plus
            | T::Minus
            | T::Bang
            | T::Tilde
            | T::Inc
            | T::Dec
            | T::Slash
            | T::SlashAssign
            | T::PrivateName => true,
            _ => false,
        }
    }

    fn parse_yield(&mut self, no_in: bool) -> R<Expr> {
        let start = self.start();
        if self.yield_pos.is_none() {
            self.yield_pos = Some(start);
        }
        self.next()?;
        if self.is(T::Semi)
            || self.can_insert_semicolon()
            || (!self.is(T::Star) && !self.starts_expr())
        {
            return Ok(self.expr(
                start,
                ExprKind::Yield {
                    argument: None,
                    delegate: false,
                },
            ));
        }
        let delegate = self.eat(T::Star)?;
        let argument = self.parse_assign(no_in)?;
        Ok(self.expr(
            start,
            ExprKind::Yield {
                argument: Some(Box::new(argument)),
                delegate,
            },
        ))
    }

    /// LeftHandSideExpression: a primary expression with its subscripts.
    pub(crate) fn parse_subscripts_expr(
        &mut self,
        cover: Option<&mut Cover>,
        no_in: bool,
    ) -> R<Expr> {
        let start = self.start();
        let mut cover = cover;
        let atom = self.parse_atom(cover.as_deref_mut(), false, no_in)?;
        if is_arrow(&atom) {
            return Ok(atom);
        }
        let result = self.parse_subscripts(atom, start, false)?;
        if let Some(c) = cover {
            if matches!(result.kind, ExprKind::Member { .. }) {
                if c.paren_assign.is_some_and(|p| p >= start) {
                    c.paren_assign = None;
                }
                if c.paren_bind.is_some_and(|p| p >= start) {
                    c.paren_bind = None;
                }
                if c.trailing_comma.is_some_and(|p| p >= start) {
                    c.trailing_comma = None;
                }
            }
        }
        Ok(result)
    }

    /// Member accesses, calls, tagged templates and optional chains after
    /// `base`. `no_calls` is set for the callee of `new`.
    pub(crate) fn parse_subscripts(&mut self, base: Expr, start: u32, no_calls: bool) -> R<Expr> {
        let maybe_async_arrow = matches!(&base.kind, ExprKind::Ident(n) if n == "async")
            && self.prev_end == base.span.end
            && !self.can_insert_semicolon()
            && base.span.end - base.span.start == 5
            && self.potential_arrow_at == Some(base.span.start);
        let mut base = base;
        let mut optional_chained = false;
        loop {
            let optional = self.is(T::QuestionDot);
            if optional {
                if no_calls {
                    return Err(self.error(
                        self.tok.span,
                        "optional chain cannot appear in the callee of new",
                    ));
                }
                self.next()?;
                optional_chained = true;
            }
            if self.eat(T::LBracket)? {
                let property = self.parse_expression(false)?;
                self.expect(T::RBracket)?;
                base = self.expr(
                    start,
                    ExprKind::Member {
                        object: Box::new(base),
                        property: MemberProperty::Computed(Box::new(property)),
                        optional,
                    },
                );
            } else if (optional && !self.is(T::LParen) && !self.is(T::Template))
                || self.eat(T::Dot)?
            {
                let property = if self.is(T::PrivateName) {
                    let span = self.tok.span;
                    let name = self.tok.name().to_string();
                    self.next()?;
                    self.use_private(&name, span)?;
                    MemberProperty::Private(Name { span, name })
                } else {
                    let (name, span) = self.parse_identifier_name()?;
                    MemberProperty::Name(Name { span, name })
                };
                base = self.expr(
                    start,
                    ExprKind::Member {
                        object: Box::new(base),
                        property,
                        optional,
                    },
                );
            } else if !no_calls && self.is(T::LParen) {
                self.next()?;
                let old_yield = self.yield_pos.take();
                let old_await = self.await_pos.take();
                let old_await_ident = self.await_ident_pos.take();
                let mut cover = Cover::default();
                let arguments = self.parse_arguments(&mut cover)?;
                if maybe_async_arrow
                    && !optional
                    && !self.can_insert_semicolon()
                    && self.is(T::Arrow)
                {
                    self.check_pattern_errors(&cover, false)?;
                    self.check_yield_await_in_params()?;
                    if let Some(pos) = self.await_ident_pos {
                        return Err(self.error_at(
                            pos,
                            "'await' cannot be a parameter of an async arrow function",
                        ));
                    }
                    self.yield_pos = old_yield;
                    self.await_pos = old_await;
                    self.await_ident_pos = old_await_ident;
                    let params_span = Span::new(base.span.end, self.prev_end);
                    self.next()?;
                    return self.parse_arrow_from_arguments(start, params_span, arguments);
                }
                self.check_expression_errors(&cover)?;
                self.yield_pos = old_yield.or(self.yield_pos);
                self.await_pos = old_await.or(self.await_pos);
                self.await_ident_pos = old_await_ident.or(self.await_ident_pos);
                if !optional && is_eval_reference(&base) {
                    self.note_direct_eval();
                }
                base = self.expr(
                    start,
                    ExprKind::Call {
                        callee: Box::new(base),
                        arguments,
                        optional,
                    },
                );
            } else if self.is(T::Template) {
                if optional || optional_chained {
                    return Err(self.error(
                        self.tok.span,
                        "tagged template cannot be used in an optional chain",
                    ));
                }
                let quasi = self.parse_template(true)?;
                base = self.expr(
                    start,
                    ExprKind::TaggedTemplate {
                        tag: Box::new(base),
                        quasi,
                    },
                );
            } else {
                break;
            }
        }
        if optional_chained {
            base = self.expr(start, ExprKind::OptionalChain(Box::new(base)));
        }
        Ok(base)
    }

    /// Arguments after `(`, through `)`.
    pub(crate) fn parse_arguments(&mut self, cover: &mut Cover) -> R<Vec<Argument>> {
        let mut args = Vec::new();
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
                let e = self.parse_assign_cover(false, cover, false)?;
                if self.is(T::Comma) && cover.trailing_comma.is_none() {
                    cover.trailing_comma = Some(self.start());
                }
                args.push(Argument::Spread(e));
            } else {
                args.push(Argument::Expr(
                    self.parse_assign_cover(false, cover, false)?,
                ));
            }
        }
        Ok(args)
    }

    /// PrimaryExpression (plus `new`, `super` and `import` forms).
    fn parse_atom(&mut self, cover: Option<&mut Cover>, new_callee: bool, no_in: bool) -> R<Expr> {
        let start = self.start();
        let can_be_arrow = self.potential_arrow_at == Some(start);
        match self.tok.kind {
            T::Name => {
                if !self.tok.escaped {
                    match self.tok.name() {
                        "super" => return self.parse_super(new_callee),
                        "this" => {
                            self.next()?;
                            self.note_this();
                            return Ok(self.expr(start, ExprKind::This));
                        }
                        "null" => {
                            self.next()?;
                            return Ok(self.expr(start, ExprKind::Null));
                        }
                        "true" | "false" => {
                            let value = self.tok.name() == "true";
                            self.next()?;
                            return Ok(self.expr(start, ExprKind::Bool(value)));
                        }
                        "function" => {
                            let f = self.parse_function_expression(start, false)?;
                            return Ok(self.expr(start, ExprKind::Function(Box::new(f))));
                        }
                        "class" => {
                            let c = self.parse_class(start, false, false)?;
                            return Ok(self.expr(start, ExprKind::Class(Box::new(c))));
                        }
                        "new" => return self.parse_new(),
                        "import" => return self.parse_import_expr(new_callee),
                        _ => {}
                    }
                }
                let escaped = self.tok.escaped;
                let (name, span) = self.parse_ident(Usage::Reference)?;
                if !escaped
                    && name == "async"
                    && !self.can_insert_semicolon()
                    && self.is_kw("function")
                {
                    let f = self.parse_function_expression(start, true)?;
                    return Ok(self.expr(start, ExprKind::Function(Box::new(f))));
                }
                if can_be_arrow && !self.can_insert_semicolon() {
                    if self.is(T::Arrow) {
                        self.next()?;
                        let param = self.ident_pattern(name, span);
                        return self.parse_arrow(start, span, vec![param], None, false, no_in);
                    }
                    if !escaped && name == "async" && self.is(T::Name) {
                        let (param, param_span) = self.parse_ident(Usage::Binding)?;
                        if self.can_insert_semicolon() || !self.is(T::Arrow) {
                            return Err(self.unexpected());
                        }
                        self.next()?;
                        let param = self.ident_pattern(param, param_span);
                        return self.parse_arrow(start, param_span, vec![param], None, true, no_in);
                    }
                }
                if name == "arguments" {
                    self.note_arguments();
                }
                Ok(self.expr(start, ExprKind::Ident(name)))
            }
            T::Number => {
                if self.strict && self.tok.legacy_octal {
                    return Err(self.error(
                        self.tok.span,
                        "legacy octal literals are not allowed in strict mode",
                    ));
                }
                let Value::Number(n) = self.tok.value else {
                    return Err(self.unexpected());
                };
                self.next()?;
                Ok(self.expr(start, ExprKind::Number(n)))
            }
            T::BigInt => {
                let Value::BigInt(digits) = self.tok.value.clone() else {
                    return Err(self.unexpected());
                };
                self.next()?;
                Ok(self.expr(start, ExprKind::BigInt(digits)))
            }
            T::String => {
                if self.strict && self.tok.legacy_octal {
                    return Err(self.error(
                        self.tok.span,
                        "legacy octal escape sequences are not allowed in strict mode",
                    ));
                }
                let value = string_value(&self.tok);
                self.next()?;
                Ok(self.expr(start, ExprKind::String(value)))
            }
            T::Slash | T::SlashAssign => self.parse_regexp(),
            T::Template => {
                let template = self.parse_template(false)?;
                Ok(self.expr(start, ExprKind::Template(template)))
            }
            T::LParen => {
                let e = self.parse_paren_and_distinguish(can_be_arrow, no_in)?;
                if let Some(c) = cover {
                    if c.paren_assign.is_none() && !is_simple_assign_target(&e) {
                        c.paren_assign = Some(start);
                    }
                    if c.paren_bind.is_none() {
                        c.paren_bind = Some(start);
                    }
                }
                Ok(e)
            }
            T::LBracket => {
                let mut own = Cover::default();
                let cover = match cover {
                    Some(c) => c,
                    None => &mut own,
                };
                self.parse_array_literal(cover)
            }
            T::LBrace => {
                let mut own = Cover::default();
                let cover = match cover {
                    Some(c) => c,
                    None => &mut own,
                };
                self.parse_object_literal(cover)
            }
            _ => Err(self.unexpected()),
        }
    }

    fn ident_pattern(&self, name: String, span: Span) -> PatternElement {
        PatternElement {
            span,
            target: Pattern {
                span,
                kind: PatternKind::Ident(name),
            },
            default: None,
        }
    }

    fn parse_regexp(&mut self) -> R<Expr> {
        let start = self.start();
        self.tok = self.lexer.read_regexp(start, self.tok.nl_before)?;
        let span = self.tok.span;
        let Value::RegExp { pattern, flags } = self.tok.value.clone() else {
            return Err(self.unexpected());
        };
        let mut seen = String::new();
        for c in flags.chars() {
            if !"dgimsuvy".contains(c) || seen.contains(c) {
                return Err(
                    self.error(span, &format!("invalid regular expression flags '{flags}'"))
                );
            }
            seen.push(c);
        }
        if seen.contains('u') && seen.contains('v') {
            return Err(self.error(
                span,
                "regular expression flags 'u' and 'v' cannot be combined",
            ));
        }
        if let Some(validate) = self.options.regexp_validator {
            if let Err(message) = validate(&pattern, &flags) {
                return Err(self.error(span, &format!("invalid regular expression: {message}")));
            }
        }
        self.next()?;
        Ok(self.expr(start, ExprKind::RegExp { pattern, flags }))
    }

    /// TemplateLiteral; the current token is its first part.
    pub(crate) fn parse_template(&mut self, tagged: bool) -> R<Template> {
        let start = self.start();
        let mut quasis = Vec::new();
        let mut expressions = Vec::new();
        loop {
            let span = self.tok.span;
            let Value::Template { cooked, raw, tail } = self.tok.value.clone() else {
                return Err(self.unexpected());
            };
            let cooked = match cooked {
                Ok(cooked) => Some(cooked),
                Err((pos, message)) => {
                    if !tagged {
                        return Err(self.error_at(pos, message));
                    }
                    None
                }
            };
            quasis.push(TemplateElement { span, cooked, raw });
            self.next()?;
            if tail {
                break;
            }
            expressions.push(self.parse_expression(false)?);
            if !self.is(T::RBrace) {
                return Err(self.unexpected());
            }
            self.tok = self
                .lexer
                .read_template_continuation(self.tok.span.start, self.tok.nl_before)?;
        }
        Ok(Template {
            span: self.span_from(start),
            quasis,
            expressions,
        })
    }

    fn parse_super(&mut self, new_callee: bool) -> R<Expr> {
        let start = self.start();
        let super_span = self.tok.span;
        self.next()?;
        match self.tok.kind {
            T::LParen if !new_callee => {
                if !self.allow_super_call() {
                    return Err(self.error(
                        super_span,
                        "'super()' is only valid in derived class constructors",
                    ));
                }
                self.next()?;
                let mut cover = Cover::default();
                let args = self.parse_arguments(&mut cover)?;
                self.check_expression_errors(&cover)?;
                self.note_this();
                Ok(self.expr(start, ExprKind::SuperCall(args)))
            }
            T::Dot | T::LBracket => {
                if !self.allow_super_property() {
                    return Err(self.error(super_span, "'super' is only valid in methods"));
                }
                self.note_this();
                let property = if self.eat(T::Dot)? {
                    let (name, span) = self.parse_identifier_name()?;
                    MemberProperty::Name(Name { span, name })
                } else {
                    self.next()?;
                    let e = self.parse_expression(false)?;
                    self.expect(T::RBracket)?;
                    MemberProperty::Computed(Box::new(e))
                };
                Ok(self.expr(start, ExprKind::SuperMember(property)))
            }
            _ => Err(self.error(super_span, "unexpected 'super'")),
        }
    }

    fn parse_new(&mut self) -> R<Expr> {
        let start = self.start();
        self.next()?;
        if self.is(T::Dot) {
            self.next()?;
            if !self.is_kw("target") {
                return Err(self.unexpected());
            }
            self.next()?;
            if !self.allow_new_target() {
                return Err(self.error(
                    self.span_from(start),
                    "new.target is only valid in functions",
                ));
            }
            return Ok(self.expr(start, ExprKind::NewTarget));
        }
        let callee_start = self.start();
        let callee = self.parse_atom(None, true, false)?;
        let callee = self.parse_subscripts(callee, callee_start, true)?;
        let arguments = if self.eat(T::LParen)? {
            let mut cover = Cover::default();
            let args = self.parse_arguments(&mut cover)?;
            self.check_expression_errors(&cover)?;
            args
        } else {
            Vec::new()
        };
        Ok(self.expr(
            start,
            ExprKind::New {
                callee: Box::new(callee),
                arguments,
            },
        ))
    }

    /// `import.meta` or `import(specifier [, options])`.
    fn parse_import_expr(&mut self, new_callee: bool) -> R<Expr> {
        let start = self.start();
        self.next()?;
        if self.eat(T::Dot)? {
            if !self.is_kw("meta") {
                return Err(self.unexpected());
            }
            self.next()?;
            if !self.module {
                return Err(self.error(
                    self.span_from(start),
                    "import.meta is only valid in modules",
                ));
            }
            return Ok(self.expr(start, ExprKind::ImportMeta));
        }
        if new_callee || !self.is(T::LParen) {
            return Err(self.error(self.span_from(start), "unexpected 'import'"));
        }
        self.next()?;
        if self.is(T::Ellipsis) {
            return Err(self.unexpected());
        }
        let specifier = self.parse_assign(false)?;
        let mut options = None;
        if self.eat(T::Comma)? && !self.is(T::RParen) {
            if self.is(T::Ellipsis) {
                return Err(self.unexpected());
            }
            options = Some(Box::new(self.parse_assign(false)?));
            self.eat(T::Comma)?;
        }
        self.expect(T::RParen)?;
        Ok(self.expr(
            start,
            ExprKind::ImportCall {
                specifier: Box::new(specifier),
                options,
            },
        ))
    }

    /// `( ... )`: a parenthesized expression or the parameters of an arrow
    /// function, decided by a following `=>`.
    fn parse_paren_and_distinguish(&mut self, can_be_arrow: bool, no_in: bool) -> R<Expr> {
        let start = self.start();
        self.next()?;
        let inner_start = self.start();
        let mut items = Vec::new();
        let mut rest = None;
        let mut first = true;
        let mut last_is_comma = false;
        let mut cover = Cover::default();
        let old_yield = self.yield_pos.take();
        let old_await = self.await_pos.take();
        while !self.is(T::RParen) {
            if !first {
                self.expect(T::Comma)?;
                if self.is(T::RParen) {
                    last_is_comma = true;
                    break;
                }
            }
            first = false;
            if self.is(T::Ellipsis) {
                let rest_start = self.start();
                self.next()?;
                rest = Some((rest_start, self.parse_binding_target()?));
                if self.is(T::Comma) {
                    return Err(self.error(self.tok.span, "a rest parameter must be last"));
                }
                break;
            }
            items.push(self.parse_assign_cover(false, &mut cover, false)?);
        }
        let inner_end = self.prev_end;
        self.expect(T::RParen)?;
        if can_be_arrow && !self.can_insert_semicolon() && self.is(T::Arrow) {
            self.check_pattern_errors(&cover, false)?;
            self.check_yield_await_in_params()?;
            self.yield_pos = old_yield;
            self.await_pos = old_await;
            let params_span = self.span_from(start);
            self.next()?;
            let mut params = Vec::new();
            for item in items {
                params.push(self.expr_to_element(item, true)?);
            }
            let rest = rest.map(|(_, pattern)| Box::new(pattern));
            return self.parse_arrow(start, params_span, params, rest, false, no_in);
        }
        if items.is_empty() || last_is_comma {
            return Err(self.error(Span::new(inner_end, inner_end), "unexpected token ')'"));
        }
        if let Some((pos, _)) = rest {
            return Err(self.error_at(pos, "unexpected '...'"));
        }
        self.check_expression_errors(&cover)?;
        self.yield_pos = old_yield.or(self.yield_pos);
        self.await_pos = old_await.or(self.await_pos);
        let inner = if items.len() == 1 {
            items.pop().unwrap()
        } else {
            Expr {
                span: Span::new(inner_start, inner_end),
                kind: ExprKind::Sequence(items),
            }
        };
        Ok(self.expr(start, ExprKind::Paren(Box::new(inner))))
    }

    /// `async ( arguments ) =>`: the call arguments become the parameters.
    fn parse_arrow_from_arguments(
        &mut self,
        start: u32,
        params_span: Span,
        args: Vec<Argument>,
    ) -> R<Expr> {
        let mut params = Vec::new();
        let mut rest = None;
        let count = args.len();
        for (i, arg) in args.into_iter().enumerate() {
            match arg {
                Argument::Expr(e) => params.push(self.expr_to_element(e, true)?),
                Argument::Spread(e) => {
                    if i + 1 != count {
                        return Err(self.error(e.span, "a rest parameter must be last"));
                    }
                    if matches!(e.kind, ExprKind::Assign { .. }) {
                        return Err(self.error(e.span, "a rest parameter cannot have a default"));
                    }
                    rest = Some(Box::new(self.expr_to_pattern(e, true)?));
                }
            }
        }
        self.parse_arrow(start, params_span, params, rest, true, false)
    }

    fn parse_array_literal(&mut self, cover: &mut Cover) -> R<Expr> {
        let start = self.start();
        self.next()?;
        let mut elements = Vec::new();
        loop {
            if self.eat(T::RBracket)? {
                break;
            }
            if self.eat(T::Comma)? {
                elements.push(ArrayElement::Hole);
                continue;
            }
            let element = if self.eat(T::Ellipsis)? {
                let e = self.parse_assign_cover(false, cover, false)?;
                if self.is(T::Comma) && cover.trailing_comma.is_none() {
                    cover.trailing_comma = Some(self.start());
                }
                ArrayElement::Spread(e)
            } else {
                ArrayElement::Expr(self.parse_assign_cover(false, cover, false)?)
            };
            elements.push(element);
            if !self.is(T::RBracket) {
                self.expect(T::Comma)?;
            }
        }
        Ok(self.expr(start, ExprKind::Array(elements)))
    }

    fn parse_object_literal(&mut self, cover: &mut Cover) -> R<Expr> {
        let start = self.start();
        self.next()?;
        let mut properties = Vec::new();
        let mut has_proto = false;
        let mut first = true;
        while !self.eat(T::RBrace)? {
            if !first {
                self.expect(T::Comma)?;
                if self.eat(T::RBrace)? {
                    break;
                }
            }
            first = false;
            let property = self.parse_object_property(cover)?;
            if let PropertyKind::Proto(_) = property.kind {
                if has_proto && cover.double_proto.is_none() {
                    cover.double_proto = Some(property.span.start);
                }
                has_proto = true;
            }
            properties.push(property);
        }
        Ok(self.expr(start, ExprKind::Object(properties)))
    }

    /// The current `async`, `get` or `set` is a modifier (a property name
    /// follows), not the property name itself.
    fn is_property_modifier(&self) -> bool {
        let Some(next) = self.peek() else {
            return false;
        };
        let starts_key = matches!(
            next.kind,
            T::Name | T::String | T::Number | T::BigInt | T::LBracket | T::PrivateName
        );
        if self.is_kw("async") {
            !next.nl_before && (starts_key || next.kind == T::Star)
        } else {
            starts_key
        }
    }

    fn parse_object_property(&mut self, cover: &mut Cover) -> R<Property> {
        let start = self.start();
        if self.eat(T::Ellipsis)? {
            let argument = self.parse_assign_cover(false, cover, false)?;
            if self.is(T::Comma) && cover.trailing_comma.is_none() {
                cover.trailing_comma = Some(self.start());
            }
            return Ok(Property {
                span: self.span_from(start),
                kind: PropertyKind::Spread(Box::new(argument)),
            });
        }
        let mut is_generator = self.eat(T::Star)?;
        let mut is_async = false;
        let mut kind = crate::ast::MethodKind::Method;
        if !is_generator
            && (self.is_kw("async") || self.is_kw("get") || self.is_kw("set"))
            && self.is_property_modifier()
        {
            let word = self.tok.name().to_string();
            match word.as_str() {
                "async" => {
                    is_async = true;
                    self.next()?;
                    is_generator = self.eat(T::Star)?;
                }
                "get" => {
                    kind = crate::ast::MethodKind::Get;
                    self.next()?;
                }
                _ => {
                    kind = crate::ast::MethodKind::Set;
                    self.next()?;
                }
            }
        }
        let key_tok = self.tok.clone();
        if key_tok.kind == T::PrivateName {
            return Err(self.unexpected());
        }
        let key = self.parse_property_key()?;
        if is_generator || is_async || kind != crate::ast::MethodKind::Method {
            if !self.is(T::LParen) {
                return Err(self.unexpected());
            }
            let function = self.parse_method(
                start,
                kind,
                is_async,
                is_generator,
                crate::ast::FunctionKind::Method,
                false,
            )?;
            return Ok(Property {
                span: self.span_from(start),
                kind: PropertyKind::Method {
                    key,
                    kind,
                    function: Box::new(function),
                },
            });
        }
        if self.eat(T::Colon)? {
            let value = self.parse_assign_cover(false, cover, false)?;
            let kind = if !matches!(key, PropertyKey::Computed(_)) && key_is_proto(&key) {
                PropertyKind::Proto(Box::new(value))
            } else {
                PropertyKind::Init {
                    key,
                    value: Box::new(value),
                    shorthand: false,
                }
            };
            return Ok(Property {
                span: self.span_from(start),
                kind,
            });
        }
        if self.is(T::LParen) {
            let function = self.parse_method(
                start,
                kind,
                false,
                false,
                crate::ast::FunctionKind::Method,
                false,
            )?;
            return Ok(Property {
                span: self.span_from(start),
                kind: PropertyKind::Method {
                    key,
                    kind,
                    function: Box::new(function),
                },
            });
        }
        // Shorthand `name` or CoverInitializedName `name = value`.
        let PropertyKey::Name(name) = &key else {
            return Err(self.unexpected());
        };
        if key_tok.kind != T::Name {
            return Err(self.unexpected());
        }
        let name = name.clone();
        self.check_identifier(&name, key_tok.span, Usage::Reference)?;
        if name == "arguments" {
            self.note_arguments();
        }
        let ident = Expr {
            span: key_tok.span,
            kind: ExprKind::Ident(name.clone()),
        };
        let value = if self.is(T::Assign) {
            if cover.shorthand_assign.is_none() {
                cover.shorthand_assign = Some(self.start());
            }
            self.next()?;
            let default = self.parse_assign(false)?;
            self.expr(
                start,
                ExprKind::Assign {
                    op: AssignOp::Assign,
                    target: Box::new(Pattern {
                        span: key_tok.span,
                        kind: PatternKind::Ident(name),
                    }),
                    value: Box::new(default),
                },
            )
        } else {
            ident
        };
        Ok(Property {
            span: self.span_from(start),
            kind: PropertyKind::Init {
                key,
                value: Box::new(value),
                shorthand: true,
            },
        })
    }

    /// PropertyName: IdentifierName, string, number, BigInt or `[expr]`.
    pub(crate) fn parse_property_key(&mut self) -> R<PropertyKey> {
        match self.tok.kind {
            T::Name => {
                let name = self.tok.name().to_string();
                self.next()?;
                Ok(PropertyKey::Name(name))
            }
            T::String => {
                if self.strict && self.tok.legacy_octal {
                    return Err(self.error(
                        self.tok.span,
                        "legacy octal escape sequences are not allowed in strict mode",
                    ));
                }
                let value = string_value(&self.tok);
                self.next()?;
                Ok(PropertyKey::String(value))
            }
            T::Number => {
                if self.strict && self.tok.legacy_octal {
                    return Err(self.error(
                        self.tok.span,
                        "legacy octal literals are not allowed in strict mode",
                    ));
                }
                let Value::Number(n) = self.tok.value else {
                    return Err(self.unexpected());
                };
                self.next()?;
                Ok(PropertyKey::Number(n))
            }
            T::BigInt => {
                let Value::BigInt(digits) = self.tok.value.clone() else {
                    return Err(self.unexpected());
                };
                self.next()?;
                Ok(PropertyKey::BigInt(digits))
            }
            T::LBracket => {
                self.next()?;
                let e = self.parse_assign(false)?;
                self.expect(T::RBracket)?;
                Ok(PropertyKey::Computed(Box::new(e)))
            }
            _ => Err(self.unexpected()),
        }
    }

    // ----- Assignment targets -----

    /// Early errors of an identifier used as an assignment or binding target.
    fn check_target_name(&self, name: &str, span: Span) -> R<()> {
        if self.strict && (name == "eval" || name == "arguments") {
            return Err(self.error(span, &format!("cannot assign to '{name}' in strict mode")));
        }
        Ok(())
    }

    /// AssignmentTargetType is simple (or web-compat: a call in sloppy mode
    /// when `allow_call`).
    pub(crate) fn check_simple_target(&self, e: &Expr, allow_call: bool) -> R<()> {
        match &e.kind {
            ExprKind::Ident(name) => self.check_target_name(name, e.span),
            ExprKind::Member { .. } | ExprKind::SuperMember(_) => Ok(()),
            ExprKind::Paren(inner) => self.check_simple_target(inner, allow_call),
            ExprKind::Call { .. } if allow_call && !self.strict => Ok(()),
            ExprKind::OptionalChain(_) => {
                Err(self.error(e.span, "an optional chain is not a valid assignment target"))
            }
            _ => Err(self.error(e.span, "invalid assignment target")),
        }
    }

    /// Converts a simple assignment target to a pattern, dropping parentheses.
    pub(crate) fn to_simple_target(&self, e: Expr, allow_call: bool) -> R<Pattern> {
        self.check_simple_target(&e, allow_call)?;
        Ok(simple_pattern(e))
    }

    /// The left side of `=` or of a for-in/of head: a destructuring pattern
    /// (from an object or array literal) or a simple target.
    pub(crate) fn to_assign_pattern(&self, e: Expr, cover: Option<&Cover>) -> R<Pattern> {
        match e.kind {
            ExprKind::Object(_) | ExprKind::Array(_) => {
                if let Some(cover) = cover {
                    self.check_pattern_errors(cover, true)?;
                }
                self.expr_to_pattern(e, false)
            }
            _ => self.to_simple_target(e, true),
        }
    }

    /// Converts an expression of a cover grammar to a pattern: an
    /// assignment pattern, or a binding pattern for arrow parameters.
    pub(crate) fn expr_to_pattern(&self, e: Expr, binding: bool) -> R<Pattern> {
        let span = e.span;
        match e.kind {
            ExprKind::Ident(name) => {
                self.check_target_name(&name, span)?;
                Ok(Pattern {
                    span,
                    kind: PatternKind::Ident(name),
                })
            }
            ExprKind::Member { .. } | ExprKind::SuperMember(_) if !binding => Ok(Pattern {
                span,
                kind: PatternKind::Expr(Box::new(e)),
            }),
            ExprKind::Paren(inner) if !binding => {
                let inner = *inner;
                if !is_simple_assign_target(&inner) {
                    return Err(self.error(span, "invalid parenthesized assignment target"));
                }
                self.to_simple_target(inner, false)
            }
            ExprKind::Object(props) => {
                let mut properties = Vec::new();
                let mut rest = None;
                let count = props.len();
                for (i, prop) in props.into_iter().enumerate() {
                    match prop.kind {
                        PropertyKind::Init {
                            key,
                            value,
                            shorthand,
                        } => properties.push(PatternProperty {
                            span: prop.span,
                            key,
                            value: self.expr_to_element(*value, binding)?,
                            shorthand,
                        }),
                        PropertyKind::Proto(value) => properties.push(PatternProperty {
                            span: prop.span,
                            key: PropertyKey::Name("__proto__".to_string()),
                            value: self.expr_to_element(*value, binding)?,
                            shorthand: false,
                        }),
                        PropertyKind::Spread(argument) => {
                            if i + 1 != count {
                                return Err(self.error(prop.span, "a rest element must be last"));
                            }
                            let argument = *argument;
                            let simple = if binding {
                                matches!(argument.kind, ExprKind::Ident(_))
                            } else {
                                matches!(
                                    argument.kind,
                                    ExprKind::Ident(_)
                                        | ExprKind::Member { .. }
                                        | ExprKind::SuperMember(_)
                                        | ExprKind::Paren(_)
                                )
                            };
                            if !simple {
                                return Err(self.error(argument.span, "invalid rest element"));
                            }
                            rest = Some(Box::new(self.expr_to_pattern(argument, binding)?));
                        }
                        PropertyKind::Method { .. } => {
                            return Err(self.error(prop.span, "invalid destructuring target"));
                        }
                    }
                }
                Ok(Pattern {
                    span,
                    kind: PatternKind::Object { properties, rest },
                })
            }
            ExprKind::Array(items) => {
                let mut elements = Vec::new();
                let mut rest = None;
                let count = items.len();
                for (i, item) in items.into_iter().enumerate() {
                    match item {
                        ArrayElement::Hole => elements.push(None),
                        ArrayElement::Expr(e) => {
                            elements.push(Some(self.expr_to_element(e, binding)?))
                        }
                        ArrayElement::Spread(e) => {
                            if i + 1 != count {
                                return Err(self.error(e.span, "a rest element must be last"));
                            }
                            if matches!(e.kind, ExprKind::Assign { .. }) {
                                return Err(
                                    self.error(e.span, "a rest element cannot have a default")
                                );
                            }
                            rest = Some(Box::new(self.expr_to_pattern(e, binding)?));
                        }
                    }
                }
                Ok(Pattern {
                    span,
                    kind: PatternKind::Array { elements, rest },
                })
            }
            ExprKind::OptionalChain(_) => {
                Err(self.error(span, "an optional chain is not a valid assignment target"))
            }
            _ => Err(self.error(span, "invalid destructuring target")),
        }
    }

    /// A pattern element: a target with an optional `= default`.
    pub(crate) fn expr_to_element(&self, e: Expr, binding: bool) -> R<PatternElement> {
        let span = e.span;
        match e.kind {
            ExprKind::Assign {
                op: AssignOp::Assign,
                target,
                value,
            } => {
                self.revalidate_pattern(&target, binding)?;
                Ok(PatternElement {
                    span,
                    target: *target,
                    default: Some(value),
                })
            }
            ExprKind::Assign { .. } => Err(self.error(span, "invalid destructuring target")),
            _ => Ok(PatternElement {
                span,
                target: self.expr_to_pattern(e, binding)?,
                default: None,
            }),
        }
    }

    /// Re-checks a pattern converted earlier (the target of a nested `=`) in
    /// a binding context: only identifiers may be bound.
    pub(crate) fn revalidate_pattern(&self, p: &Pattern, binding: bool) -> R<()> {
        match &p.kind {
            PatternKind::Ident(name) => self.check_target_name(name, p.span),
            PatternKind::Expr(_) => {
                if binding {
                    Err(self.error(p.span, "invalid binding target"))
                } else {
                    Ok(())
                }
            }
            PatternKind::Array { elements, rest } => {
                for element in elements.iter().flatten() {
                    self.revalidate_pattern(&element.target, binding)?;
                }
                if let Some(rest) = rest {
                    self.revalidate_pattern(rest, binding)?;
                }
                Ok(())
            }
            PatternKind::Object { properties, rest } => {
                for property in properties {
                    self.revalidate_pattern(&property.value.target, binding)?;
                }
                if let Some(rest) = rest {
                    self.revalidate_pattern(rest, binding)?;
                }
                Ok(())
            }
        }
    }
}

/// A checked simple target as a pattern: identifiers become bindings,
/// everything else (property accesses, web-compat calls) is kept as an
/// expression.
fn simple_pattern(e: Expr) -> Pattern {
    match e.kind {
        ExprKind::Paren(inner) => simple_pattern(*inner),
        ExprKind::Ident(name) => Pattern {
            span: e.span,
            kind: PatternKind::Ident(name),
        },
        _ => Pattern {
            span: e.span,
            kind: PatternKind::Expr(Box::new(e)),
        },
    }
}
