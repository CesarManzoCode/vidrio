//! ECMAScript language values (ECMA-262 §6.1) and property keys.

use crate::gc::{Atom, HeapBigInt, JsString, Object, Symbol, Trace, Tracer};

/// An ECMAScript language value. Heap values are handles into the arenas
/// of [`crate::gc::Heap`].
///
/// `PartialEq` is structural (handles compare by index, numbers with `==`);
/// use `ops::same_value` and friends for the ECMAScript comparisons.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Boolean(bool),
    Number(f64),
    String(JsString),
    Symbol(Symbol),
    BigInt(HeapBigInt),
    Object(Object),
}

/// The type of a value, as in the `Type(x)` of the specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    Undefined,
    Null,
    Boolean,
    Number,
    String,
    Symbol,
    BigInt,
    Object,
}

impl Value {
    pub fn value_type(self) -> ValueType {
        match self {
            Value::Undefined => ValueType::Undefined,
            Value::Null => ValueType::Null,
            Value::Boolean(_) => ValueType::Boolean,
            Value::Number(_) => ValueType::Number,
            Value::String(_) => ValueType::String,
            Value::Symbol(_) => ValueType::Symbol,
            Value::BigInt(_) => ValueType::BigInt,
            Value::Object(_) => ValueType::Object,
        }
    }

    pub fn is_undefined(self) -> bool {
        matches!(self, Value::Undefined)
    }

    pub fn is_null(self) -> bool {
        matches!(self, Value::Null)
    }

    /// `undefined` or `null`.
    pub fn is_nullish(self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    pub fn is_object(self) -> bool {
        matches!(self, Value::Object(_))
    }

    pub fn as_object(self) -> Option<Object> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_number(self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_string(self) -> Option<JsString> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_symbol(self) -> Option<Symbol> {
        match self {
            Value::Symbol(s) => Some(s),
            _ => None,
        }
    }

    /// `undefined` for `None`, the object otherwise.
    pub fn from_object(o: Option<Object>) -> Value {
        match o {
            Some(o) => Value::Object(o),
            None => Value::Undefined,
        }
    }

    /// `null` for `None`, the object otherwise (prototype slots).
    pub fn object_or_null(o: Option<Object>) -> Value {
        match o {
            Some(o) => Value::Object(o),
            None => Value::Null,
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Boolean(b)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Value {
        Value::Number(n)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Value {
        Value::Number(n as f64)
    }
}

impl From<u32> for Value {
    fn from(n: u32) -> Value {
        Value::Number(n as f64)
    }
}

impl From<Object> for Value {
    fn from(o: Object) -> Value {
        Value::Object(o)
    }
}

impl From<JsString> for Value {
    fn from(s: JsString) -> Value {
        Value::String(s)
    }
}

impl From<Symbol> for Value {
    fn from(s: Symbol) -> Value {
        Value::Symbol(s)
    }
}

/// Largest array index, 2^32 - 2 (ECMA-262 §6.1.7).
pub const MAX_ARRAY_INDEX: u32 = u32::MAX - 1;

/// A property key. Strings that are canonical array indices are always
/// stored as `Index`, every other string as an interned atom, so two keys
/// are the same property exactly when they are equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PropertyKey {
    /// An array index: 0 ..= 2^32 - 2.
    Index(u32),
    String(Atom),
    Symbol(Symbol),
}

impl PropertyKey {
    pub fn is_index(self) -> bool {
        matches!(self, PropertyKey::Index(_))
    }

    pub fn as_index(self) -> Option<u32> {
        match self {
            PropertyKey::Index(i) => Some(i),
            _ => None,
        }
    }

    pub fn is_symbol(self) -> bool {
        matches!(self, PropertyKey::Symbol(_))
    }

    /// String keys in the sense of the specification (indices included).
    pub fn is_string(self) -> bool {
        !self.is_symbol()
    }
}

impl From<Atom> for PropertyKey {
    fn from(a: Atom) -> PropertyKey {
        PropertyKey::String(a)
    }
}

impl From<Symbol> for PropertyKey {
    fn from(s: Symbol) -> PropertyKey {
        PropertyKey::Symbol(s)
    }
}

/// A value that can be held weakly (`CanBeHeldWeakly`): an object or a
/// symbol that is not registered with `Symbol.for`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WeakKey {
    Object(Object),
    Symbol(Symbol),
}

impl WeakKey {
    pub fn to_value(self) -> Value {
        match self {
            WeakKey::Object(o) => Value::Object(o),
            WeakKey::Symbol(s) => Value::Symbol(s),
        }
    }
}

impl Trace for WeakKey {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.weak_key(*self);
    }
}

/// A symbol. `private` symbols are Private Names: they never appear as
/// property keys, only as keys of private elements.
#[derive(Debug)]
pub struct SymbolData {
    pub description: Option<JsString>,
    /// Created by `Symbol.for` (lives in the agent's registry).
    pub registered: bool,
    pub private: bool,
}

impl Trace for SymbolData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(d) = self.description {
            tracer.string(d);
        }
    }
}
