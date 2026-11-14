//! Storage of environment records: a chain of slot vectors. `None` is an
//! uninitialized binding (temporal dead zone). The binding semantics live
//! in the compiler and interpreter; mapped `arguments` objects and module
//! namespaces read and write these slots directly.
//!
//! Every environment knows the names of its slots ([`ScopeNames`]) so that
//! code compiled without static knowledge of the chain (direct `eval`,
//! bodies of `with`, global code) can look bindings up by name.

use std::collections::HashMap;
use std::rc::Rc;

use crate::gc::{Atom, Env, Object, Trace, Tracer};
use crate::value::Value;

/// The binding starts uninitialized (`let`, `const`, `class`, parameters
/// of functions with parameter expressions).
pub const BIND_TDZ: u8 = 1;
/// Immutable binding whose assignment throws in any mode (`const`, the
/// inner name binding of a class).
pub const BIND_CONST: u8 = 2;
/// Immutable binding whose assignment is ignored in sloppy mode and throws
/// in strict mode (the name of a named function expression).
pub const BIND_SLOPPY_CONST: u8 = 4;
/// Lexically declared (conflicts with `var` declarations of `eval` code).
pub const BIND_LEXICAL: u8 = 8;

/// Names and flags of the slots of a declarative environment.
#[derive(Debug, Default)]
pub struct ScopeNames {
    pub names: Vec<Atom>,
    pub flags: Vec<u8>,
    /// The environment receives `var` declarations (function bodies,
    /// static blocks, strict `eval` code): the VariableEnvironment.
    pub var_scope: bool,
    /// The environment of a `catch (e)` clause with a simple parameter
    /// (Annex B.3.4: `var e` inside `eval` does not conflict).
    pub simple_catch: bool,
    /// Hidden slots of a function environment that arrow functions and
    /// direct `eval` read: the `this` value, `new.target` and the active
    /// function (for `super`). Filled when the environment is entered.
    pub this_slot: Option<u32>,
    pub new_target_slot: Option<u32>,
    pub function_slot: Option<u32>,
}

impl ScopeNames {
    pub fn find(&self, name: Atom) -> Option<usize> {
        self.names.iter().position(|n| *n == name)
    }
}

impl Trace for ScopeNames {
    fn trace(&self, tracer: &mut Tracer) {
        for a in &self.names {
            tracer.atom(*a);
        }
    }
}

/// The global Environment Record: the object record (the global object)
/// plus a declarative record whose bindings are the `slots` of the
/// environment, indexed by `lexical`.
pub struct GlobalEnvData {
    pub object: Object,
    pub this: Value,
    pub lexical: HashMap<Atom, u32>,
    pub flags: Vec<u8>,
    /// `[[VarNames]]`: names bound by `var` and function declarations of
    /// global code.
    pub var_names: Vec<Atom>,
}

pub enum EnvKind {
    Declarative(Option<Rc<ScopeNames>>),
    /// Object Environment Record (`with`, or a non-global object binding).
    Object {
        object: Object,
        with: bool,
    },
    Global(Box<GlobalEnvData>),
}

pub struct EnvData {
    pub outer: Option<Env>,
    pub slots: Vec<Option<Value>>,
    pub kind: EnvKind,
    /// Deletable `var` and function bindings that a non-strict direct
    /// `eval` added to this VariableEnvironment.
    pub eval_vars: Option<Box<Vec<(Atom, Value)>>>,
}

impl EnvData {
    pub fn declarative(outer: Option<Env>, slots: usize) -> EnvData {
        EnvData {
            outer,
            slots: vec![None; slots],
            kind: EnvKind::Declarative(None),
            eval_vars: None,
        }
    }

    /// A declarative environment for a compiled scope: slots of bindings
    /// without [`BIND_TDZ`] start as `undefined`.
    pub fn for_scope(outer: Option<Env>, names: Rc<ScopeNames>) -> EnvData {
        let slots = names
            .flags
            .iter()
            .map(|f| {
                if f & BIND_TDZ != 0 {
                    None
                } else {
                    Some(Value::Undefined)
                }
            })
            .collect();
        EnvData {
            outer,
            slots,
            kind: EnvKind::Declarative(Some(names)),
            eval_vars: None,
        }
    }

    pub fn names(&self) -> Option<&Rc<ScopeNames>> {
        match &self.kind {
            EnvKind::Declarative(n) => n.as_ref(),
            _ => None,
        }
    }

    /// A global environment over `object` whose `this` is `this`.
    pub fn global(object: Object, this: Value) -> EnvData {
        EnvData {
            outer: None,
            slots: Vec::new(),
            kind: EnvKind::Global(Box::new(GlobalEnvData {
                object,
                this,
                lexical: HashMap::new(),
                flags: Vec::new(),
                var_names: Vec::new(),
            })),
            eval_vars: None,
        }
    }

    /// An object environment (`with` when `with` is true).
    pub fn object(outer: Option<Env>, object: Object, with: bool) -> EnvData {
        EnvData {
            outer,
            slots: Vec::new(),
            kind: EnvKind::Object { object, with },
            eval_vars: None,
        }
    }

    pub fn is_var_scope(&self) -> bool {
        match &self.kind {
            EnvKind::Declarative(Some(n)) => n.var_scope,
            EnvKind::Global(_) => true,
            _ => false,
        }
    }
}

impl Trace for EnvData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(outer) = self.outer {
            tracer.env(outer);
        }
        for v in self.slots.iter().flatten() {
            tracer.value(*v);
        }
        match &self.kind {
            EnvKind::Declarative(Some(names)) => names.trace(tracer),
            EnvKind::Declarative(None) => {}
            EnvKind::Object { object, .. } => {
                tracer.object(*object);
            }
            EnvKind::Global(g) => {
                tracer.object(g.object);
                tracer.value(g.this);
                for a in g.lexical.keys().chain(g.var_names.iter()) {
                    tracer.atom(*a);
                }
            }
        }
        if let Some(vars) = &self.eval_vars {
            for (a, v) in vars.iter() {
                tracer.atom(*a);
                tracer.value(*v);
            }
        }
    }
}
