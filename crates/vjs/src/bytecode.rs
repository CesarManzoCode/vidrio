//! Bytecode of vjs: a stack machine whose instructions are the [`Op`]
//! values of a [`Code`] block. Jump targets are instruction indices.
//!
//! Every code block owns its constants (strings, BigInts), the atoms of the
//! names it uses, the code of the functions defined in it and the layouts
//! of the declarative environments it creates. Frames also have numbered
//! registers (`LoadReg`/`StoreReg`) that the compiler allocates for state
//! that must survive across statements: iterators of `for-of`, the pending
//! completion of `finally`, the discriminant of `switch`.

use std::any::Any;
use std::rc::Rc;

use crate::ast::Span;
use crate::env::ScopeNames;
use crate::function::CodeBlock;
use crate::gc::{Atom, Trace, Tracer};
use crate::value::Value;

/// Which hidden binding of a function environment an instruction reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hidden {
    This,
    NewTarget,
    Function,
}

/// Kind of a method definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodOp {
    Method,
    Get,
    Set,
}

/// The name a class constructor receives (ClassDefinitionEvaluation's
/// `className`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassName {
    /// Index in `atoms`.
    Atom(u32),
    /// A property key held in a register (computed keys).
    Reg(u32),
    Empty,
}

/// One instruction. Stack effects are written `[before] → [after]`.
#[derive(Clone, Copy, Debug)]
pub enum Op {
    // ----- constants and stack -----
    Undefined,
    Null,
    True,
    False,
    Int(i32),
    Num(f64),
    /// `consts[i]`.
    Const(u32),
    Pop,
    PopN(u32),
    Dup,
    /// `[a b] → [a b a b]`.
    Dup2,
    Swap,
    /// `[a b c] → [c a b]`.
    RotUp3,
    /// `[a b c d] → [d a b c]`.
    RotUp4,
    LoadReg(u32),
    /// Pops into the register.
    StoreReg(u32),
    /// Pops into the completion value register of the frame.
    SetCompletion,
    /// Sets the completion value to `undefined`.
    ClearCompletion,
    LoadCompletion,

    // ----- environments and bindings -----
    /// New declarative environment with the layout `scopes[i]`.
    PushScope(u32),
    PopScope,
    /// CreatePerIterationEnvironment: replaces the current environment by
    /// a copy.
    CopyScope,
    /// `[obj] → []`: object environment of a `with` statement.
    PushWith,
    /// Fills the hidden `this`, `new.target` and function slots of the
    /// current environment from the frame.
    InitHidden,
    /// Reads a binding `hops` environments out; ReferenceError in TDZ.
    GetLocal {
        hops: u32,
        slot: u32,
    },
    /// `[v] → [v]`: assigns a mutable binding (ReferenceError in TDZ).
    SetLocal {
        hops: u32,
        slot: u32,
    },
    /// `[v] → []`: initializes a binding.
    InitLocal {
        hops: u32,
        slot: u32,
    },
    /// Assignment to an immutable binding: ReferenceError in TDZ, else
    /// TypeError.
    ThrowConstAssign {
        hops: u32,
        slot: u32,
    },
    /// ReferenceError if the binding is in its TDZ.
    CheckLocal {
        hops: u32,
        slot: u32,
    },
    /// Dynamic lookup of `atoms[i]`; ReferenceError if unresolvable.
    GetName(u32),
    /// Dynamic lookup for `typeof`: `undefined` if unresolvable.
    GetNameOrUndefined(u32),
    /// `[] → [f this]`: callee lookup with the `with` object as `this`.
    GetNameWithThis(u32),
    /// `[] → [base]`: resolves a reference before the right-hand side is
    /// evaluated (the binding object, `null` for a declarative binding,
    /// `undefined` if unresolvable).
    ResolveRef(u32),
    /// `[base] → [base v]`.
    GetRef(u32),
    /// `[base v] → [v]`.
    SetRef {
        name: u32,
        strict: bool,
    },
    /// `[v] → []`: initializes a binding found dynamically (global
    /// lexical declarations).
    InitName(u32),
    DeleteName(u32),
    /// Dynamic lookup of a hidden binding (`this` of eval or of arrow
    /// functions without a static path).
    GetHidden(Hidden),
    /// `[v] → [v]`: BindThisValue on the hidden `this` found dynamically.
    BindThisDyn,
    /// `[v] → [v]`: BindThisValue on a static hidden slot.
    BindThis {
        hops: u32,
        slot: u32,
    },
    /// The frame's `this` (ReferenceError if not initialized).
    LoadThis,
    LoadNewTarget,
    /// The active function object.
    LoadFunction,

    // ----- function prologue -----
    GetArg(u32),
    /// Array of the arguments from index `i` on.
    RestArgs(u32),
    CreateArguments {
        mapped: bool,
    },
    /// Function object for `functions[i]` closing over the current
    /// environment.
    Closure(u32),
    /// Named function expression: closure with its own name binding.
    ClosureNamed(u32),
    /// `[key] → [key f]`: closure named after a computed property key.
    ClosureKeyName(u32),
    GlobalDeclInit(u32),
    EvalDeclInit(u32),
    /// Start of a base class constructor: InitializeInstanceElements.
    InitThisFields,

    // ----- operators -----
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
    /// `[name obj] → [bool]`: `#x in obj`.
    PrivateIn,
    Neg,
    ToNumber,
    Not,
    BitNot,
    Typeof,
    ToNumeric,
    Inc,
    Dec,
    ToPropertyKey,
    /// ToString (template literals).
    ToStringOp,
    /// `[a b] → [a+b]` for two strings.
    Concat,
    /// TypeError unless the top of the stack is an object (keeps it).
    CheckObject(u32),
    /// TypeError if the top of the stack is `undefined` or `null`.
    RequireObjectCoercible,

    // ----- jumps -----
    Jump(u32),
    JumpIfTrue(u32),
    JumpIfFalse(u32),
    /// Jumps without popping if the value is truthy; pops otherwise.
    JumpIfTrueKeep(u32),
    JumpIfFalseKeep(u32),
    /// Jumps without popping if the value is not `undefined`/`null`; pops
    /// otherwise (`??`).
    JumpIfNotNullishKeep(u32),
    /// Jumps if the value is not `undefined` (keeps it).
    JumpIfNotUndefined(u32),
    /// Jumps if the value `depth` entries below the top is `undefined` or
    /// `null` (optional chains).
    JumpIfNullishAt {
        depth: u32,
        target: u32,
    },

    // ----- properties -----
    /// `[obj] → [v]`.
    GetNamed(u32),
    /// `[obj key] → [v]`.
    GetComputed,
    /// `[obj] → [obj v]`.
    GetNamedKeep(u32),
    /// `[obj key] → [obj key v]`.
    GetComputedKeep,
    /// `[obj v] → [v]`.
    SetNamed {
        name: u32,
        strict: bool,
    },
    /// `[obj key v] → [v]`.
    SetComputed {
        strict: bool,
    },
    /// `[obj] → [f obj]`.
    GetMethodNamed(u32),
    /// `[obj key] → [f obj]`.
    GetMethodComputed,
    DeleteNamed {
        name: u32,
        strict: bool,
    },
    DeleteComputed {
        strict: bool,
    },
    /// `[func] → [home.[[GetPrototypeOf]]()]`.
    SuperBase,
    /// `[this base] → [v]`.
    GetSuperNamed(u32),
    /// `[this key base] → [v]`.
    GetSuperComputed,
    /// `[this base v] → [v]`.
    SetSuperNamed {
        name: u32,
        strict: bool,
    },
    /// `[this key base v] → [v]`.
    SetSuperComputed {
        strict: bool,
    },
    /// `[this base] → [f this]`.
    GetSuperMethodNamed(u32),
    /// `[this key base] → [f this]`.
    GetSuperMethodComputed,
    /// `[obj name] → [v]`.
    GetPrivate,
    /// `[obj name] → [obj name v]`.
    GetPrivateKeep,
    /// `[obj name v] → [v]`.
    SetPrivate,
    /// `[obj name] → [f obj]`.
    GetPrivateMethod,

    // ----- literals -----
    NewObject,
    NewArray,
    /// `[arr v] → [arr]`.
    ArrayPush,
    /// `[arr] → [arr]`: an elision.
    ArrayHole,
    /// `[arr iterable] → [arr]`.
    ArraySpread,
    /// `[obj key v] → [obj]`: CreateDataPropertyOrThrow.
    DefineField,
    /// `[obj key] → [obj]`: method, getter or setter of an object literal.
    DefineMethod {
        func: u32,
        kind: MethodOp,
    },
    /// `[obj v] → [obj]`: `__proto__: v`.
    SetProtoLiteral,
    /// `[obj src] → [obj]`.
    CopyDataProps,
    /// `[src keys] → [obj]`: object rest with the excluded keys array.
    CopyRest,
    GetTemplateObject(u32),
    /// RegExpCreate(consts[pattern], consts[flags]).
    RegExp {
        pattern: u32,
        flags: u32,
    },

    // ----- calls -----
    /// `[f this a0..an-1] → [r]`.
    Call(u32),
    /// `[f this arr] → [r]`.
    CallArray,
    /// Possible direct `eval`: `[f this a0..an-1] → [r]`.
    CallEval {
        argc: u32,
        site: u32,
    },
    CallEvalArray {
        site: u32,
    },
    /// `[f a0..an-1] → [o]`.
    New(u32),
    /// `[f arr] → [o]`.
    NewSpread,
    /// `[func] → [ctor]`: GetSuperConstructor.
    GetSuperConstructor,
    /// `[ctor newTarget a0..an-1] → [this]`.
    SuperCall(u32),
    /// `[ctor newTarget arr] → [this]`.
    SuperCallSpread,
    /// `[ctor newTarget] → [this]`: the default derived constructor, which
    /// passes its own arguments on.
    SuperCallForward,
    /// `[this func] → [this]`: InitializeInstanceElements(this, func).
    InitInstanceElements,
    Return,

    // ----- exceptions -----
    Throw,
    /// Installs a handler: on a throw the stack is cut back to its current
    /// height, the environment restored, the exception pushed and control
    /// moves to the target.
    PushHandler(u32),
    PopHandler,
    /// Throws a TypeError with message `consts[i]`.
    ThrowTypeError(u32),
    /// Throws a ReferenceError with message `consts[i]`.
    ThrowReferenceError(u32),
    /// End of a `finally` block (or of a disposal block): the completion
    /// kind is in register `kind` (0 normal, 1 throw, n ≥ 2 the exit
    /// `exit_tables[table][n - 2]`) and its value in register `value`.
    EndFinally {
        kind: u32,
        value: u32,
        table: u32,
    },

    // ----- iteration -----
    /// `[v] → []`: GetIterator into registers `iter` and `iter + 1`.
    GetIterator {
        iter: u32,
        is_async: bool,
    },
    /// `[] → [v]`, or jumps when done.
    IteratorStep {
        iter: u32,
        done: u32,
    },
    /// `[] → [result]`: calls `next` (async iteration awaits it).
    IteratorCallNext {
        iter: u32,
    },
    /// `[result] → [v]`, or pops and jumps when done.
    IteratorResult {
        done: u32,
    },
    /// IteratorClose with a normal completion.
    IteratorClose {
        iter: u32,
    },
    /// `[exc] → throw`: IteratorClose with a throw completion.
    IteratorCloseThrow {
        iter: u32,
    },
    /// `[] → [result]` of `return()`, or jumps if there is none.
    AsyncIteratorCloseCall {
        iter: u32,
        skip: u32,
    },
    /// `[] → [result]` of `return()`; jumps if there is none or it threw.
    AsyncIteratorCloseThrowCall {
        iter: u32,
        skip: u32,
    },
    /// `[v] → []`: iterator for destructuring into `base`, `base + 1` and
    /// the done flag `base + 2`.
    DestructIter {
        base: u32,
    },
    DestructStep {
        base: u32,
    },
    DestructRest {
        base: u32,
    },
    DestructClose {
        base: u32,
    },
    /// `[exc] → throw`.
    DestructCloseThrow {
        base: u32,
    },
    /// `[v] → []`: iterator of `for-in` in `reg`; jumps if `v` is nullish.
    ForInPrepare {
        reg: u32,
        skip: u32,
    },
    ForInNext {
        reg: u32,
        done: u32,
    },

    // ----- generators and async -----
    /// End of the prologue of a generator: creates the generator object,
    /// suspends and returns it.
    InitialYield,
    /// `[v] → suspend`; resumed as `[v mode]`.
    Yield,
    /// Yield of `yield*` in a sync generator: the result object as is.
    YieldRaw,
    /// AsyncGeneratorYield.
    AsyncGenYield,
    /// `[v mode] → [v]`: mode 0 continues, 1 throws, 2 jumps to `ret`.
    ResumeKind {
        ret: u32,
    },
    /// `[v] → suspend` until the promise settles; resumed as `[v mode]`.
    Await,
    /// `[v mode] → [v]` or throw.
    AwaitResume,
    /// Step of `yield*`: calls next/throw/return according to the mode in
    /// register `iter + 3` with the value in `iter + 2`.
    YieldStarCall {
        iter: u32,
        no_throw: u32,
        ret: u32,
    },
    /// `[r] → [r]` (sync) or `[value]` (async); jumps to `done` or `ret`
    /// with `[value]` when the inner iterator is done.
    YieldStarResult {
        iter: u32,
        done: u32,
        ret: u32,
        is_async: bool,
    },

    // ----- classes -----
    /// `[superclass] → [protoParent ctorParent]`.
    ClassHeritage,
    ClassNoHeritage,
    /// `[protoParent ctorParent] → [F proto]`.
    CreateClass {
        func: u32,
        name: ClassName,
        derived: bool,
    },
    /// `[F proto key] → [F proto]`.
    DefineClassMethod {
        func: u32,
        kind: MethodOp,
        is_static: bool,
    },
    /// `[F proto name] → [F proto]`.
    DefinePrivateMethod {
        func: u32,
        kind: MethodOp,
        is_static: bool,
    },
    /// `[F proto key] → [F proto]`: instance field; `func` is `u32::MAX`
    /// without initializer.
    AddField {
        func: u32,
        private: bool,
    },
    /// `[F key] → [F]`: DefineField(F, static field).
    DefineStaticField {
        func: u32,
        private: bool,
    },
    /// `[F] → [F]`: runs a static block.
    StaticBlock(u32),
    /// `[desc] → [name]`: a new Private Name.
    NewPrivateName,

    // ----- explicit resource management -----
    /// `[] → []`: an empty disposable resource stack in `reg`.
    UsingInit(u32),
    /// `[v] → [v]`: AddDisposableResource.
    AddDisposable {
        reg: u32,
        is_async: bool,
    },
    /// Calls the dispose method of the newest resource and pushes its
    /// result, or jumps to `done` when none is left.
    DisposeStep {
        reg: u32,
        done: u32,
    },
    /// `[err] → []`: combines a disposal error into the pending completion
    /// (registers `kind`, `value`).
    DisposeError {
        kind: u32,
        value: u32,
    },

    Debugger,
}

impl Op {
    /// The jump targets of the instruction, for patching.
    pub fn targets_mut(&mut self) -> Vec<&mut u32> {
        match self {
            Op::Jump(t)
            | Op::JumpIfTrue(t)
            | Op::JumpIfFalse(t)
            | Op::JumpIfTrueKeep(t)
            | Op::JumpIfFalseKeep(t)
            | Op::JumpIfNotNullishKeep(t)
            | Op::JumpIfNotUndefined(t)
            | Op::PushHandler(t) => vec![t],
            Op::JumpIfNullishAt { target, .. } => vec![target],
            Op::IteratorStep { done, .. }
            | Op::IteratorResult { done }
            | Op::ForInNext { done, .. }
            | Op::DisposeStep { done, .. } => vec![done],
            Op::AsyncIteratorCloseCall { skip, .. }
            | Op::AsyncIteratorCloseThrowCall { skip, .. }
            | Op::ForInPrepare { skip, .. } => vec![skip],
            Op::ResumeKind { ret } => vec![ret],
            Op::YieldStarCall { no_throw, ret, .. } => vec![no_throw, ret],
            Op::YieldStarResult { done, ret, .. } => vec![done, ret],
            _ => Vec::new(),
        }
    }
}

/// What a direct `eval` at a call site may contain (PerformEval's
/// `inFunction`, `inMethod`, `inDerivedConstructor`,
/// `inClassFieldInitializer`) and the strictness of the caller.
#[derive(Clone, Debug, Default)]
pub struct EvalSite {
    pub strict: bool,
    pub new_target: bool,
    pub super_property: bool,
    pub super_call: bool,
    pub class_field_initializer: bool,
    pub private_names: Vec<String>,
}

/// The declarations instantiated at run time by GlobalDeclarationInstantiation
/// or EvalDeclarationInstantiation.
#[derive(Clone, Debug, Default)]
pub struct Declarations {
    /// Var names that are not function names, in order.
    pub var_names: Vec<Atom>,
    /// Functions to initialize: name and index in `functions` (the last
    /// declaration of a name wins).
    pub functions: Vec<(Atom, u32)>,
    /// Lexically declared names and whether each is constant.
    pub lexical: Vec<(Atom, bool)>,
}

impl Trace for Declarations {
    fn trace(&self, tracer: &mut Tracer) {
        for a in &self.var_names {
            tracer.atom(*a);
        }
        for (a, _) in &self.functions {
            tracer.atom(*a);
        }
        for (a, _) in &self.lexical {
            tracer.atom(*a);
        }
    }
}

/// A tagged template site: its process-unique id (the key of the realm's
/// `[[TemplateMap]]`) and the indices of its strings in `consts`
/// (`None` for a cooked string that is undefined).
#[derive(Clone, Debug)]
pub struct TemplateSite {
    pub id: usize,
    pub cooked: Vec<Option<u32>>,
    pub raw: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeKind {
    Script,
    Eval,
    /// Any function body, including class field initializers and static
    /// blocks.
    Function,
}

/// Facts about a function needed to create and call its function objects.
#[derive(Clone, Debug, Default)]
pub struct FunctionFlags {
    pub is_arrow: bool,
    pub is_method: bool,
    pub is_async: bool,
    pub is_generator: bool,
    pub is_class_constructor: bool,
    pub is_derived: bool,
    /// A plain `function` that can be called with `new`.
    pub is_constructor: bool,
}

/// The source text of a function (`Function.prototype.toString`).
#[derive(Clone, Debug)]
pub struct SourceText {
    pub source: Rc<str>,
    pub span: Span,
}

impl SourceText {
    pub fn text(&self) -> &str {
        self.span.text(&self.source)
    }
}

/// A compiled script, eval body or function body.
pub struct Code {
    pub kind: CodeKind,
    pub ops: Vec<Op>,
    pub consts: Vec<Value>,
    pub atoms: Vec<Atom>,
    pub functions: Vec<Rc<Code>>,
    pub scopes: Vec<Rc<ScopeNames>>,
    pub exit_tables: Vec<Vec<u32>>,
    pub eval_sites: Vec<EvalSite>,
    pub decls: Vec<Declarations>,
    pub templates: Vec<TemplateSite>,
    /// Mapped `arguments`: parameter index → slot of the function
    /// environment (the first scope).
    pub arg_map: Vec<Option<u32>>,
    pub num_regs: u32,
    pub strict: bool,
    pub flags: FunctionFlags,
    /// The name given by SetFunctionName when the function object is
    /// created (`None`: the name is set later from a computed key).
    pub name: Option<Atom>,
    /// The environment layout holding the name of a named function
    /// expression.
    pub name_scope: Option<Rc<ScopeNames>>,
    /// ExpectedArgumentCount.
    pub length: u32,
    pub source: Option<SourceText>,
}

impl Trace for Code {
    fn trace(&self, tracer: &mut Tracer) {
        for v in &self.consts {
            tracer.value(*v);
        }
        for a in &self.atoms {
            tracer.atom(*a);
        }
        for f in &self.functions {
            f.trace(tracer);
        }
        for s in &self.scopes {
            s.trace(tracer);
        }
        for d in &self.decls {
            d.trace(tracer);
        }
        if let Some(n) = self.name {
            tracer.atom(n);
        }
        if let Some(s) = &self.name_scope {
            s.trace(tracer);
        }
    }
}

impl CodeBlock for Code {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_rc(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
}

impl Code {
    pub fn is_generator(&self) -> bool {
        self.flags.is_generator
    }

    pub fn is_async(&self) -> bool {
        self.flags.is_async
    }
}
