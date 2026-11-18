//! The static description of the IDL definitions that the generator of
//! `crates/vidrio/bindgen` emits into `OUT_DIR` (see [`super::generated`]):
//! interfaces, members, dictionaries, enumerations and callback functions,
//! with types already resolved. The runtime of this module reads it to
//! build interface objects and to convert values.

pub use super::generated::Op;

/// Index into [`super::generated::INTERFACES`] (or the dictionary, enum or
/// callback table, by context).
pub type Id = u16;

/// A resolved IDL type (Web IDL §2.13). Annotations wrap the type they
/// apply to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ty {
    Any,
    Undefined,
    Boolean,
    Byte,
    Octet,
    Short,
    UnsignedShort,
    Long,
    UnsignedLong,
    LongLong,
    UnsignedLongLong,
    Float,
    UnrestrictedFloat,
    Double,
    UnrestrictedDouble,
    BigInt,
    DomString,
    ByteString,
    UsvString,
    Object,
    Symbol,
    Interface(Id),
    CallbackInterface(Id),
    Dictionary(Id),
    Enum(Id),
    Callback(Id),
    Buffer(BufferKind),
    Nullable(&'static Ty),
    Sequence(&'static Ty),
    FrozenArray(&'static Ty),
    ObservableArray(&'static Ty),
    Record(&'static Ty, &'static Ty),
    Promise(&'static Ty),
    Union(&'static [Ty]),
    EnforceRange(&'static Ty),
    Clamp(&'static Ty),
    LegacyNullToEmptyString(&'static Ty),
    AllowShared(&'static Ty),
    /// A name defined outside the copied IDL files; converted as `any`.
    Unknown(&'static str),
}

impl Ty {
    /// The type without annotations.
    pub fn inner(self) -> Ty {
        match self {
            Ty::EnforceRange(t)
            | Ty::Clamp(t)
            | Ty::LegacyNullToEmptyString(t)
            | Ty::AllowShared(t) => t.inner(),
            t => t,
        }
    }

    /// True for a nullable type, or a union with a nullable member
    /// ("includes a nullable type", Web IDL §2.13.24).
    pub fn includes_nullable(self) -> bool {
        match self.inner() {
            Ty::Nullable(_) => true,
            Ty::Union(members) => members.iter().any(|m| m.includes_nullable()),
            _ => false,
        }
    }
}

/// The buffer source types (Web IDL §2.13.32–§2.13.35).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferKind {
    ArrayBuffer,
    SharedArrayBuffer,
    DataView,
    Int8Array,
    Uint8Array,
    Uint8ClampedArray,
    Int16Array,
    Uint16Array,
    Int32Array,
    Uint32Array,
    BigInt64Array,
    BigUint64Array,
    Float16Array,
    Float32Array,
    Float64Array,
}

impl BufferKind {
    pub fn name(self) -> &'static str {
        match self {
            BufferKind::ArrayBuffer => "ArrayBuffer",
            BufferKind::SharedArrayBuffer => "SharedArrayBuffer",
            BufferKind::DataView => "DataView",
            BufferKind::Int8Array => "Int8Array",
            BufferKind::Uint8Array => "Uint8Array",
            BufferKind::Uint8ClampedArray => "Uint8ClampedArray",
            BufferKind::Int16Array => "Int16Array",
            BufferKind::Uint16Array => "Uint16Array",
            BufferKind::Int32Array => "Int32Array",
            BufferKind::Uint32Array => "Uint32Array",
            BufferKind::BigInt64Array => "BigInt64Array",
            BufferKind::BigUint64Array => "BigUint64Array",
            BufferKind::Float16Array => "Float16Array",
            BufferKind::Float32Array => "Float32Array",
            BufferKind::Float64Array => "Float64Array",
        }
    }
}

/// A default value or a constant's value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DefaultValue {
    Null,
    Undefined,
    Bool(bool),
    Number(f64),
    Str(&'static str),
    EmptySequence,
    EmptyDictionary,
}

#[derive(Debug)]
pub struct Argument {
    pub name: &'static str,
    pub ty: Ty,
    pub optional: bool,
    pub variadic: bool,
    pub default: Option<DefaultValue>,
}

/// One overload of an operation or constructor; `index` is its position
/// in the overload set, handed to the implementation.
#[derive(Debug)]
pub struct Overload {
    pub op: Op,
    pub index: u8,
    pub args: &'static [Argument],
    pub ret: Ty,
}

/// A regular or static operation, a constructor or a legacy factory
/// function, with its overload set.
#[derive(Debug)]
pub struct Operation {
    pub name: &'static str,
    pub overloads: &'static [Overload],
    /// The shortest argument count of the effective overload set.
    pub length: u32,
    pub is_static: bool,
    pub unforgeable: bool,
    pub unscopable: bool,
    pub secure_context: bool,
    pub new_object: bool,
}

#[derive(Debug)]
pub struct Attribute {
    pub name: &'static str,
    pub ty: Ty,
    pub getter: Op,
    pub setter: Option<Op>,
    pub readonly: bool,
    pub is_static: bool,
    pub unforgeable: bool,
    pub unscopable: bool,
    pub secure_context: bool,
    pub replaceable: bool,
    pub put_forwards: Option<&'static str>,
    pub same_object: bool,
    pub lenient_this: bool,
    pub lenient_setter: bool,
}

#[derive(Debug)]
pub struct Constant {
    pub name: &'static str,
    pub value: DefaultValue,
}

/// The special operations of a legacy platform object (Web IDL §3.9).
/// Getters carry their return type, setters the type of their value.
#[derive(Debug)]
pub struct Special {
    pub indexed_getter: Option<(Op, Ty)>,
    pub indexed_setter: Option<(Op, Ty)>,
    pub named_getter: Option<(Op, Ty)>,
    pub named_setter: Option<(Op, Ty)>,
    pub named_deleter: Option<Op>,
    /// Returns the number of supported property indices (they are
    /// `0..count`).
    pub index_count: Option<Op>,
    /// Returns the supported property names as a sequence of strings.
    pub supported_names: Option<Op>,
}

impl Special {
    pub fn is_legacy_platform_object(&self) -> bool {
        self.indexed_getter.is_some() || self.named_getter.is_some()
    }
}

/// `iterable`, `maplike` or `setlike` declarations.
#[derive(Debug)]
pub enum Iterable {
    None,
    /// A value iterator (the interface has an indexed getter).
    Value(Ty),
    /// A pair iterator: `pairs` returns a sequence of `[key, value]`
    /// sequences.
    Pair {
        key: Ty,
        value: Ty,
        pairs: Op,
    },
    Maplike {
        key: Ty,
        value: Ty,
        readonly: bool,
    },
    Setlike {
        value: Ty,
        readonly: bool,
    },
}

#[derive(Debug)]
pub enum Stringifier {
    None,
    /// `stringifier attribute`: the attribute's getter.
    Attribute(Op),
    /// `stringifier;` or a stringifier operation.
    Operation(Op),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Interface,
    CallbackInterface,
    Namespace,
}

#[derive(Debug)]
pub struct Interface {
    pub name: &'static str,
    pub kind: Kind,
    pub parent: Option<Id>,
    /// Exposed in `Window`.
    pub exposed: bool,
    pub secure_context: bool,
    pub global: bool,
    pub no_interface_object: bool,
    pub constants: &'static [Constant],
    pub attributes: &'static [Attribute],
    pub operations: &'static [Operation],
    pub constructor: Option<&'static Operation>,
    pub legacy_factories: &'static [Operation],
    pub window_aliases: &'static [&'static str],
    pub special: Special,
    pub iterable: Iterable,
    pub stringifier: Stringifier,
    pub legacy_unenumerable_named_properties: bool,
    pub legacy_override_builtins: bool,
}

#[derive(Debug)]
pub struct DictMember {
    pub name: &'static str,
    pub ty: Ty,
    pub required: bool,
    pub default: Option<DefaultValue>,
}

/// A dictionary; `members` are sorted by name (Web IDL §2.7).
#[derive(Debug)]
pub struct Dictionary {
    pub name: &'static str,
    pub parent: Option<Id>,
    pub members: &'static [DictMember],
}

#[derive(Debug)]
pub struct Enumeration {
    pub name: &'static str,
    pub values: &'static [&'static str],
}

#[derive(Debug)]
pub struct CallbackFunction {
    pub name: &'static str,
    pub ret: Ty,
    pub args: &'static [Argument],
    pub legacy_treat_non_object_as_null: bool,
}
