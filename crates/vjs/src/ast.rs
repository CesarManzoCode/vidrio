//! Abstract syntax tree produced by the parser. Every node carries the byte
//! range of its source text; functions and classes keep the exact range that
//! `Function.prototype.toString` returns.

use std::fmt;

/// Byte range `[start, end)` in the UTF-8 source text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }

    /// The source text the span covers.
    pub fn text(self, source: &str) -> &str {
        &source[self.start as usize..self.end as usize]
    }
}

/// 1-based line and column (in code points) of a byte offset. Lines end at
/// LF, CR, CRLF, LS and PS.
pub fn line_column(source: &str, offset: u32) -> (u32, u32) {
    let mut line = 1;
    let mut column = 1;
    let mut chars = source[..(offset as usize).min(source.len())]
        .chars()
        .peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                line += 1;
                column = 1;
            }
            '\n' | '\u{2028}' | '\u{2029}' => {
                line += 1;
                column = 1;
            }
            _ => column += 1,
        }
    }
    (line, column)
}

/// An early (parse-time) error: always reported as a `SyntaxError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
    pub span: Span,
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SyntaxError: {} ({}:{})",
            self.message, self.line, self.column
        )
    }
}

impl std::error::Error for SyntaxError {}

/// UTF-16 string value (string literals may hold lone surrogates).
pub type Utf16 = Vec<u16>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    Script,
    Module,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub span: Span,
    pub goal: Goal,
    /// Strict mode code: modules, or a `"use strict"` directive.
    pub strict: bool,
    pub body: Vec<Stmt>,
    /// Facts about the top-level code (not nested functions).
    pub scope: ScopeInfo,
}

/// Facts the compiler needs about a function or top-level body. Arrow
/// functions report their uses here and also propagate them to the enclosing
/// non-arrow function, whose `this`, `arguments` and `new.target` they share.
#[derive(Clone, Debug, Default)]
pub struct ScopeInfo {
    /// A direct call `eval(...)` appears (in this body or a nested arrow).
    pub has_direct_eval: bool,
    /// The identifier `arguments` is referenced.
    pub uses_arguments: bool,
    /// `this` or `super` is used.
    pub uses_this: bool,
    /// Var-scoped names (`var` and, at function top level, functions).
    pub var_names: Vec<String>,
    /// Lexically scoped names at the top level of the body.
    pub lexical_names: Vec<String>,
}

/// An identifier with its StringValue (escapes resolved).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub span: Span,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub span: Span,
    pub kind: StmtKind,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Empty,
    Debugger,
    Expr(Box<Expr>),
    /// A directive of a directive prologue (`"use strict";`); `raw` is the
    /// literal's source text including quotes.
    Directive {
        value: Utf16,
        raw: String,
    },
    Block(Vec<Stmt>),
    Var(VarDecl),
    Function(Box<Function>),
    Class(Box<Class>),
    If {
        test: Box<Expr>,
        consequent: Box<Stmt>,
        alternate: Option<Box<Stmt>>,
    },
    For {
        init: Option<ForInit>,
        test: Option<Box<Expr>>,
        update: Option<Box<Expr>>,
        body: Box<Stmt>,
    },
    ForIn {
        left: ForHead,
        right: Box<Expr>,
        body: Box<Stmt>,
    },
    ForOf {
        left: ForHead,
        right: Box<Expr>,
        body: Box<Stmt>,
        is_await: bool,
    },
    While {
        test: Box<Expr>,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        test: Box<Expr>,
    },
    Continue(Option<Name>),
    Break(Option<Name>),
    Return(Option<Box<Expr>>),
    With {
        object: Box<Expr>,
        body: Box<Stmt>,
    },
    Switch {
        discriminant: Box<Expr>,
        cases: Vec<SwitchCase>,
    },
    Labeled {
        label: Name,
        body: Box<Stmt>,
    },
    Throw(Box<Expr>),
    Try {
        block: Vec<Stmt>,
        handler: Option<CatchClause>,
        finalizer: Option<Vec<Stmt>>,
    },
    Import(Box<ImportDecl>),
    Export(Box<ExportDecl>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
    Using,
    AwaitUsing,
}

#[derive(Clone, Debug)]
pub struct VarDecl {
    pub span: Span,
    pub kind: VarKind,
    pub declarations: Vec<Declarator>,
}

#[derive(Clone, Debug)]
pub struct Declarator {
    pub span: Span,
    pub target: Pattern,
    pub init: Option<Box<Expr>>,
}

#[derive(Clone, Debug)]
pub enum ForInit {
    Var(VarDecl),
    Expr(Box<Expr>),
}

/// Left side of `for-in` / `for-of`: a declaration with one binding and no
/// initializer (except the Annex B `for (var x = 1 in o)`), or an assignment
/// target.
#[derive(Clone, Debug)]
pub enum ForHead {
    Var(VarDecl),
    Target(Pattern),
}

#[derive(Clone, Debug)]
pub struct SwitchCase {
    pub span: Span,
    /// `None` for `default`.
    pub test: Option<Box<Expr>>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct CatchClause {
    pub span: Span,
    pub param: Option<Pattern>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Ident(String),
    This,
    Null,
    Bool(bool),
    Number(f64),
    /// BigInt literal digits as written, without separators and without the
    /// `n` suffix, keeping a `0x`/`0o`/`0b` prefix.
    BigInt(String),
    String(Utf16),
    RegExp {
        pattern: String,
        flags: String,
    },
    Template(Template),
    TaggedTemplate {
        tag: Box<Expr>,
        quasi: Template,
    },
    Array(Vec<ArrayElement>),
    Object(Vec<Property>),
    Function(Box<Function>),
    Arrow(Box<Function>),
    Class(Box<Class>),
    /// A parenthesized expression, kept because it changes early errors
    /// (assignment targets, `delete`, `??` mixing, directives).
    Paren(Box<Expr>),
    Unary {
        op: UnaryOp,
        argument: Box<Expr>,
    },
    Update {
        op: UpdateOp,
        prefix: bool,
        argument: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Logical {
        op: LogicalOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `#x in obj`.
    PrivateIn {
        name: Name,
        right: Box<Expr>,
    },
    Assign {
        op: AssignOp,
        target: Box<Pattern>,
        value: Box<Expr>,
    },
    Conditional {
        test: Box<Expr>,
        consequent: Box<Expr>,
        alternate: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        arguments: Vec<Argument>,
        /// `f?.()`.
        optional: bool,
    },
    SuperCall(Vec<Argument>),
    New {
        callee: Box<Expr>,
        arguments: Vec<Argument>,
    },
    Member {
        object: Box<Expr>,
        property: MemberProperty,
        /// `a?.b`.
        optional: bool,
    },
    SuperMember(MemberProperty),
    /// The whole of an optional chain `a?.b.c`: the short-circuit boundary.
    OptionalChain(Box<Expr>),
    Sequence(Vec<Expr>),
    Yield {
        argument: Option<Box<Expr>>,
        delegate: bool,
    },
    Await(Box<Expr>),
    NewTarget,
    ImportMeta,
    ImportCall {
        specifier: Box<Expr>,
        options: Option<Box<Expr>>,
    },
}

#[derive(Clone, Debug)]
pub enum MemberProperty {
    Name(Name),
    Computed(Box<Expr>),
    Private(Name),
}

#[derive(Clone, Debug)]
pub enum Argument {
    Expr(Expr),
    Spread(Expr),
}

#[derive(Clone, Debug)]
pub enum ArrayElement {
    Hole,
    Expr(Expr),
    Spread(Expr),
}

#[derive(Clone, Debug)]
pub struct Template {
    pub span: Span,
    pub quasis: Vec<TemplateElement>,
    pub expressions: Vec<Expr>,
}

#[derive(Clone, Debug)]
pub struct TemplateElement {
    pub span: Span,
    /// `None` when the element has an invalid escape (tagged templates only).
    pub cooked: Option<Utf16>,
    /// TemplateRawStr: source text with CR and CRLF normalized to LF.
    pub raw: String,
}

#[derive(Clone, Debug)]
pub enum PropertyKey {
    /// IdentifierName (including reserved words) with escapes resolved.
    Name(String),
    String(Utf16),
    Number(f64),
    BigInt(String),
    Computed(Box<Expr>),
    /// `#name` (class elements only).
    Private(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    Method,
    Get,
    Set,
}

#[derive(Clone, Debug)]
pub struct Property {
    pub span: Span,
    pub kind: PropertyKind,
}

#[derive(Clone, Debug)]
pub enum PropertyKind {
    /// `key: value` or shorthand `key`.
    Init {
        key: PropertyKey,
        value: Box<Expr>,
        shorthand: bool,
    },
    /// `__proto__: value` (not computed, not shorthand): sets [[Prototype]].
    Proto(Box<Expr>),
    Method {
        key: PropertyKey,
        kind: MethodKind,
        function: Box<Function>,
    },
    Spread(Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Minus,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateOp {
    Increment,
    Decrement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    NotEq,
    StrictEq,
    StrictNotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    In,
    InstanceOf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
    Coalesce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    And,
    Or,
    Coalesce,
}

/// Binding and assignment patterns. `Expr` targets (member expressions, and
/// the web-compat call expressions of sloppy mode) only occur in assignment
/// patterns.
#[derive(Clone, Debug)]
pub struct Pattern {
    pub span: Span,
    pub kind: PatternKind,
}

#[derive(Clone, Debug)]
pub enum PatternKind {
    Ident(String),
    Expr(Box<Expr>),
    Array {
        elements: Vec<Option<PatternElement>>,
        rest: Option<Box<Pattern>>,
    },
    Object {
        properties: Vec<PatternProperty>,
        rest: Option<Box<Pattern>>,
    },
}

#[derive(Clone, Debug)]
pub struct PatternElement {
    pub span: Span,
    pub target: Pattern,
    pub default: Option<Box<Expr>>,
}

#[derive(Clone, Debug)]
pub struct PatternProperty {
    pub span: Span,
    pub key: PropertyKey,
    pub value: PatternElement,
    pub shorthand: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    /// `function` declarations and expressions.
    Normal,
    Arrow,
    /// Object literal and class methods (including accessors).
    Method,
    ClassConstructor,
    DerivedConstructor,
}

#[derive(Clone, Debug)]
pub struct Function {
    /// The source text of the function (`Function.prototype.toString`).
    pub span: Span,
    pub name: Option<Name>,
    pub kind: FunctionKind,
    pub is_async: bool,
    pub is_generator: bool,
    pub params: Params,
    pub body: FunctionBody,
    /// The function's code is strict mode code.
    pub strict: bool,
    /// The body has a `"use strict"` directive.
    pub use_strict_directive: bool,
    pub scope: ScopeInfo,
}

#[derive(Clone, Debug, Default)]
pub struct Params {
    pub span: Span,
    pub items: Vec<PatternElement>,
    pub rest: Option<Box<Pattern>>,
    /// IsSimpleParameterList: only plain identifiers.
    pub is_simple: bool,
}

#[derive(Clone, Debug)]
pub enum FunctionBody {
    Block(Vec<Stmt>),
    /// Concise arrow body.
    Expr(Box<Expr>),
}

#[derive(Clone, Debug)]
pub struct Class {
    /// The whole class source text (`Function.prototype.toString`).
    pub span: Span,
    pub name: Option<Name>,
    pub heritage: Option<Box<Expr>>,
    pub constructor: Option<Box<Function>>,
    pub elements: Vec<ClassElement>,
}

#[derive(Clone, Debug)]
pub struct ClassElement {
    pub span: Span,
    pub is_static: bool,
    pub kind: ClassElementKind,
}

#[derive(Clone, Debug)]
pub enum ClassElementKind {
    Method {
        key: PropertyKey,
        kind: MethodKind,
        function: Box<Function>,
    },
    /// A field; the initializer is evaluated as the body of a method whose
    /// facts are in `scope`.
    Field {
        key: PropertyKey,
        value: Option<Box<Expr>>,
        scope: ScopeInfo,
    },
    /// `static { ... }`.
    StaticBlock { body: Vec<Stmt>, scope: ScopeInfo },
}

#[derive(Clone, Debug)]
pub struct ModuleRequest {
    pub span: Span,
    pub specifier: Utf16,
    /// Import attributes `with { key: "value" }`, in source order.
    pub attributes: Vec<(Utf16, Utf16)>,
}

/// ModuleExportName: an IdentifierName or a well-formed string literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportName {
    pub span: Span,
    pub name: String,
    pub is_string: bool,
}

#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub span: Span,
    pub specifiers: Vec<ImportSpecifier>,
    pub source: ModuleRequest,
}

#[derive(Clone, Debug)]
pub enum ImportSpecifier {
    Default(Name),
    Namespace(Name),
    Named { imported: ExportName, local: Name },
}

#[derive(Clone, Debug)]
pub enum ExportDecl {
    /// `export { a as b }` or `export { a } from "m"`.
    Named {
        specifiers: Vec<ExportSpecifier>,
        source: Option<ModuleRequest>,
    },
    /// `export * from "m"` or `export * as ns from "m"`.
    All {
        exported: Option<ExportName>,
        source: ModuleRequest,
    },
    /// `export var|let|const|using|function|class ...`.
    Declaration(Stmt),
    DefaultFunction(Box<Function>),
    DefaultClass(Box<Class>),
    DefaultExpr(Box<Expr>),
}

#[derive(Clone, Debug)]
pub struct ExportSpecifier {
    pub span: Span,
    pub local: ExportName,
    pub exported: ExportName,
}
