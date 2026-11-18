//! IDL values (Web IDL §2.13) as the native implementation sees them, and
//! the invocation of one member.

use vjs::{Object, Value};

use super::generated::Op;
use super::meta::Id;
use super::platform::PlatformRef;

/// An IDL value. Integer types arrive as `Int` (and `unsigned long long`
/// as `UInt`), floating point types as `Double`, strings as Rust strings
/// (`USVString` already scalar values), interface, callback, buffer and
/// promise types as their object, enumerations as their string.
#[derive(Clone, Debug, PartialEq)]
pub enum IdlValue {
    /// An optional argument that was not passed and has no default.
    Missing,
    Undefined,
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Double(f64),
    BigInt(Value),
    String(String),
    Object(Object),
    /// `any`, `symbol` and types defined outside the copied IDL files.
    Any(Value),
    Sequence(Vec<IdlValue>),
    Record(Vec<(String, IdlValue)>),
    Dictionary(Box<Dict>),
}

/// A dictionary value: the members that are present, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Dict {
    pub id: Id,
    pub members: Vec<(&'static str, IdlValue)>,
}

impl Dict {
    pub fn get(&self, name: &str) -> Option<&IdlValue> {
        self.members
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.get(name)? {
            IdlValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn str(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            IdlValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn object(&self, name: &str) -> Option<Object> {
        self.get(name).and_then(IdlValue::as_object)
    }
}

impl IdlValue {
    pub fn is_missing(&self) -> bool {
        matches!(self, IdlValue::Missing)
    }

    /// Missing, `undefined` or `null`.
    pub fn is_nullish(&self) -> bool {
        matches!(
            self,
            IdlValue::Missing
                | IdlValue::Undefined
                | IdlValue::Null
                | IdlValue::Any(Value::Null | Value::Undefined)
        )
    }

    pub fn as_bool(&self) -> bool {
        matches!(self, IdlValue::Bool(true))
    }

    pub fn as_opt_bool(&self) -> Option<bool> {
        match self {
            IdlValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> f64 {
        match self {
            IdlValue::Int(i) => *i as f64,
            IdlValue::UInt(u) => *u as f64,
            IdlValue::Double(d) => *d,
            IdlValue::Bool(b) => f64::from(u8::from(*b)),
            _ => 0.0,
        }
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            IdlValue::Int(i) => *i,
            IdlValue::UInt(u) => *u as i64,
            IdlValue::Double(d) => *d as i64,
            _ => 0,
        }
    }

    pub fn as_u64(&self) -> u64 {
        match self {
            IdlValue::Int(i) => *i as u64,
            IdlValue::UInt(u) => *u,
            IdlValue::Double(d) => *d as u64,
            _ => 0,
        }
    }

    pub fn as_usize(&self) -> usize {
        self.as_u64() as usize
    }

    pub fn as_str(&self) -> &str {
        match self {
            IdlValue::String(s) => s,
            _ => "",
        }
    }

    /// The string, or `None` for `null` (nullable string types).
    pub fn as_opt_str(&self) -> Option<&str> {
        match self {
            IdlValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<Object> {
        match self {
            IdlValue::Object(o) | IdlValue::Any(Value::Object(o)) => Some(*o),
            _ => None,
        }
    }

    /// The value as an ECMAScript value, for values that are one already
    /// (strings need the heap: see [`super::convert::to_js_any`]).
    pub fn as_value(&self) -> Value {
        match self {
            IdlValue::Any(v) | IdlValue::BigInt(v) => *v,
            IdlValue::Object(o) => Value::Object(*o),
            IdlValue::Null => Value::Null,
            IdlValue::Bool(b) => Value::Boolean(*b),
            IdlValue::Int(_) | IdlValue::UInt(_) | IdlValue::Double(_) => {
                Value::Number(self.as_f64())
            }
            _ => Value::Undefined,
        }
    }

    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            IdlValue::Dictionary(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_seq(&self) -> &[IdlValue] {
        match self {
            IdlValue::Sequence(s) => s,
            _ => &[],
        }
    }

    pub fn string(s: impl Into<String>) -> IdlValue {
        IdlValue::String(s.into())
    }

    pub fn opt_string(s: Option<impl Into<String>>) -> IdlValue {
        match s {
            Some(s) => IdlValue::String(s.into()),
            None => IdlValue::Null,
        }
    }
}

/// One call of a member: the operation (with the index of the chosen
/// overload), the platform object it was called on (`None` for static
/// members and constructors), the converted arguments (every declared
/// argument, then the rest of a variadic one) and, for constructors, the
/// `NewTarget`.
pub struct Invocation {
    pub op: Op,
    pub overload: u8,
    /// The interface the member was found on (for constructors, the
    /// interface being constructed).
    pub iface: Id,
    pub this: Option<PlatformRef>,
    pub args: Vec<IdlValue>,
    pub new_target: Option<Object>,
}

impl Invocation {
    pub fn arg(&self, i: usize) -> &IdlValue {
        self.args.get(i).unwrap_or(&IdlValue::Missing)
    }

    pub fn str(&self, i: usize) -> String {
        self.arg(i).as_str().to_string()
    }

    pub fn opt_str(&self, i: usize) -> Option<String> {
        self.arg(i).as_opt_str().map(str::to_string)
    }

    pub fn bool(&self, i: usize) -> bool {
        self.arg(i).as_bool()
    }

    pub fn u32(&self, i: usize) -> u32 {
        self.arg(i).as_u64() as u32
    }

    pub fn usize(&self, i: usize) -> usize {
        self.arg(i).as_usize()
    }

    pub fn object(&self, i: usize) -> Option<Object> {
        self.arg(i).as_object()
    }

    /// The `this` object.
    pub fn this_object(&self) -> Option<Object> {
        self.this.as_ref().map(|p| p.object)
    }
}
