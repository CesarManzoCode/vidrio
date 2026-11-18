//! Conversions between ECMAScript values and IDL values (Web IDL §3.2).

use vjs::buffer::TypedArrayKind;
use vjs::function::is_callable;
use vjs::internal;
use vjs::object::ObjectKind;
use vjs::ops::{self, IntegrityLevel, Numeric};
use vjs::{Agent, JsResult, Object, PropertyKey, Value};

use super::generated::{CALLBACKS, DICTIONARIES, ENUMS, INTERFACES};
use super::meta::{BufferKind, DefaultValue, Id, Ty};
use super::platform::platform_ref;
use super::value::{Dict, IdlValue};

/// Annotations in effect while converting.
#[derive(Clone, Copy, Debug, Default)]
struct Flags {
    enforce_range: bool,
    clamp: bool,
    null_to_empty: bool,
    allow_shared: bool,
}

/// Converts `v` to an IDL value of type `ty`.
pub fn to_idl(agent: &mut Agent, v: Value, ty: Ty) -> JsResult<IdlValue> {
    convert(agent, v, ty, Flags::default())
}

/// ToString as Rust text (lone surrogates become U+FFFD).
pub fn to_rust_string(agent: &mut Agent, v: Value) -> JsResult<String> {
    let s = ops::to_string(agent, v)?;
    Ok(agent.heap.string_to_rust(s))
}

fn type_name(ty: Ty) -> String {
    match ty.inner() {
        Ty::Interface(id) | Ty::CallbackInterface(id) => INTERFACES[id as usize].name.to_string(),
        Ty::Dictionary(id) => DICTIONARIES[id as usize].name.to_string(),
        Ty::Enum(id) => ENUMS[id as usize].name.to_string(),
        Ty::Callback(id) => CALLBACKS[id as usize].name.to_string(),
        Ty::Buffer(k) => k.name().to_string(),
        t => format!("{t:?}"),
    }
}

fn convert(agent: &mut Agent, v: Value, ty: Ty, f: Flags) -> JsResult<IdlValue> {
    match ty {
        Ty::EnforceRange(t) => convert(
            agent,
            v,
            *t,
            Flags {
                enforce_range: true,
                ..f
            },
        ),
        Ty::Clamp(t) => convert(agent, v, *t, Flags { clamp: true, ..f }),
        Ty::LegacyNullToEmptyString(t) => convert(
            agent,
            v,
            *t,
            Flags {
                null_to_empty: true,
                ..f
            },
        ),
        Ty::AllowShared(t) => convert(
            agent,
            v,
            *t,
            Flags {
                allow_shared: true,
                ..f
            },
        ),
        Ty::Any | Ty::Unknown(_) => Ok(IdlValue::Any(v)),
        Ty::Undefined => Ok(IdlValue::Undefined),
        Ty::Boolean => Ok(IdlValue::Bool(ops::to_boolean(agent, v))),
        Ty::Byte => integer(agent, v, 8, true, f),
        Ty::Octet => integer(agent, v, 8, false, f),
        Ty::Short => integer(agent, v, 16, true, f),
        Ty::UnsignedShort => integer(agent, v, 16, false, f),
        Ty::Long => integer(agent, v, 32, true, f),
        Ty::UnsignedLong => integer(agent, v, 32, false, f),
        Ty::LongLong => integer(agent, v, 64, true, f),
        Ty::UnsignedLongLong => integer(agent, v, 64, false, f),
        Ty::Float => {
            let x = ops::to_number(agent, v)?;
            if !x.is_finite() {
                return agent.type_error("The provided float value is non-finite.");
            }
            let y = x as f32;
            if y.is_infinite() {
                return agent.type_error("The provided float value is out of range.");
            }
            Ok(IdlValue::Double(f64::from(y)))
        }
        Ty::UnrestrictedFloat => {
            let x = ops::to_number(agent, v)?;
            Ok(IdlValue::Double(f64::from(x as f32)))
        }
        Ty::Double => {
            let x = ops::to_number(agent, v)?;
            if !x.is_finite() {
                return agent.type_error("The provided double value is non-finite.");
            }
            Ok(IdlValue::Double(x))
        }
        Ty::UnrestrictedDouble => Ok(IdlValue::Double(ops::to_number(agent, v)?)),
        Ty::BigInt => Ok(IdlValue::BigInt(Value::BigInt(ops::to_bigint(agent, v)?))),
        Ty::DomString | Ty::UsvString => {
            if f.null_to_empty && v == Value::Null {
                return Ok(IdlValue::String(String::new()));
            }
            Ok(IdlValue::String(to_rust_string(agent, v)?))
        }
        Ty::ByteString => {
            let s = ops::to_string(agent, v)?;
            let units = agent.heap.string_units(s).to_vec();
            if units.iter().any(|u| *u > 0xFF) {
                return agent.type_error("The string contains characters outside Latin-1.");
            }
            Ok(IdlValue::String(
                units.into_iter().map(|u| char::from(u as u8)).collect(),
            ))
        }
        Ty::Object => match v {
            Value::Object(o) => Ok(IdlValue::Object(o)),
            _ => agent.type_error("The value is not an object."),
        },
        Ty::Symbol => match v {
            Value::Symbol(_) => Ok(IdlValue::Any(v)),
            _ => agent.type_error("The value is not a symbol."),
        },
        Ty::Interface(id) => match platform_ref(agent, v) {
            Some(p) if p.get().implements(id) => Ok(IdlValue::Object(p.object)),
            _ => agent.type_error(&format!(
                "The value is not of type '{}'.",
                INTERFACES[id as usize].name
            )),
        },
        Ty::CallbackInterface(_) => match v {
            Value::Object(o) => Ok(IdlValue::Object(o)),
            _ => agent.type_error("The value is not an object."),
        },
        Ty::Callback(id) => {
            if is_callable(agent, v) {
                return Ok(IdlValue::Object(v.as_object().expect("callable")));
            }
            if CALLBACKS[id as usize].legacy_treat_non_object_as_null && !v.is_object() {
                return Ok(IdlValue::Null);
            }
            agent.type_error("The value is not a function.")
        }
        Ty::Dictionary(id) => dictionary(agent, v, id),
        Ty::Enum(id) => {
            let s = to_rust_string(agent, v)?;
            let def = &ENUMS[id as usize];
            if def.values.contains(&s.as_str()) {
                Ok(IdlValue::String(s))
            } else {
                agent.type_error(&format!(
                    "The provided value '{s}' is not a valid enum value of type {}.",
                    def.name
                ))
            }
        }
        Ty::Buffer(kind) => match v {
            Value::Object(o) if buffer_matches(agent, o, kind, f.allow_shared) => {
                Ok(IdlValue::Object(o))
            }
            _ => agent.type_error(&format!("The value is not of type '{}'.", kind.name())),
        },
        Ty::Nullable(t) => {
            if v.is_nullish() {
                return Ok(IdlValue::Null);
            }
            if let Ty::Callback(id) = t.inner() {
                if CALLBACKS[id as usize].legacy_treat_non_object_as_null && !v.is_object() {
                    return Ok(IdlValue::Null);
                }
            }
            convert(agent, v, *t, f)
        }
        Ty::Sequence(t) | Ty::FrozenArray(t) | Ty::ObservableArray(t) => {
            if !v.is_object() {
                return agent.type_error("The value is not iterable.");
            }
            let key = PropertyKey::Symbol(agent.symbols.iterator);
            let Some(method) = ops::get_method(agent, v, key)? else {
                return agent.type_error("The value is not iterable.");
            };
            sequence(agent, v, *t, method)
        }
        Ty::Record(k, t) => record(agent, v, *k, *t),
        Ty::Promise(_) => {
            let ctor = agent.intrinsic(vjs::realm::Intrinsic::Promise);
            let p = vjs::interp::promise::promise_resolve(agent, ctor, v)?;
            Ok(IdlValue::Object(p.as_object().expect("a promise")))
        }
        Ty::Union(members) => union(agent, v, members, f),
    }
}

/// ConvertToInt(V, bitLength, signedness) (Web IDL §3.2.4.9).
fn integer(agent: &mut Agent, v: Value, bits: u32, signed: bool, f: Flags) -> JsResult<IdlValue> {
    let mut x = ops::to_number(agent, v)?;
    if x == 0.0 {
        x = 0.0;
    }
    let (lower, upper) = if bits == 64 {
        let max = 9_007_199_254_740_991.0;
        if signed { (-max, max) } else { (0.0, max) }
    } else if signed {
        let half = 2f64.powi(bits as i32 - 1);
        (-half, half - 1.0)
    } else {
        (0.0, 2f64.powi(bits as i32) - 1.0)
    };
    let make = |x: f64| {
        if bits == 64 && !signed {
            IdlValue::UInt(x as u64)
        } else {
            IdlValue::Int(x as i64)
        }
    };
    if f.enforce_range {
        if !x.is_finite() {
            return agent.type_error("The value is not a finite number.");
        }
        x = x.trunc();
        if x < lower || x > upper {
            return agent.type_error("The value is outside the range of the type.");
        }
        return Ok(make(x + 0.0));
    }
    if !x.is_nan() && f.clamp {
        x = x.clamp(lower, upper).round_ties_even();
        return Ok(make(x + 0.0));
    }
    if !x.is_finite() || x == 0.0 {
        return Ok(make(0.0));
    }
    let x = x.trunc();
    let modulus = 2f64.powi(bits as i32);
    let mut r = (x % modulus) as i128;
    if r < 0 {
        r += 1i128 << bits;
    }
    if signed && r >= 1i128 << (bits - 1) {
        r -= 1i128 << bits;
    }
    Ok(if bits == 64 && !signed {
        IdlValue::UInt(r as u64)
    } else {
        IdlValue::Int(r as i64)
    })
}

/// The IDL value of a default value of type `ty`.
pub fn default_value(agent: &mut Agent, d: DefaultValue, ty: Ty) -> JsResult<IdlValue> {
    Ok(match d {
        DefaultValue::Null => IdlValue::Null,
        DefaultValue::Undefined => IdlValue::Undefined,
        DefaultValue::Bool(b) => IdlValue::Bool(b),
        DefaultValue::Number(n) => {
            let mut t = ty.inner();
            while let Ty::Nullable(i) = t {
                t = i.inner();
            }
            match t {
                Ty::Byte
                | Ty::Octet
                | Ty::Short
                | Ty::UnsignedShort
                | Ty::Long
                | Ty::UnsignedLong
                | Ty::LongLong => IdlValue::Int(n as i64),
                Ty::UnsignedLongLong => IdlValue::UInt(n as u64),
                _ => IdlValue::Double(n),
            }
        }
        DefaultValue::Str(s) => IdlValue::String(s.to_string()),
        DefaultValue::EmptySequence => IdlValue::Sequence(Vec::new()),
        DefaultValue::EmptyDictionary => return to_idl(agent, Value::Undefined, ty),
    })
}

/// The dictionaries of `id`'s inheritance chain, least derived first.
pub fn dictionary_chain(id: Id) -> Vec<Id> {
    let mut chain = Vec::new();
    let mut next = Some(id);
    while let Some(i) = next {
        chain.push(i);
        next = DICTIONARIES[i as usize].parent;
    }
    chain.reverse();
    chain
}

/// Converting to a dictionary (Web IDL §3.2.17).
fn dictionary(agent: &mut Agent, v: Value, id: Id) -> JsResult<IdlValue> {
    let object = match v {
        Value::Undefined | Value::Null => None,
        Value::Object(o) => Some(o),
        _ => {
            return agent.type_error(&format!(
                "The value is not of type '{}'.",
                DICTIONARIES[id as usize].name
            ));
        }
    };
    let mut members = Vec::new();
    for d in dictionary_chain(id) {
        for m in DICTIONARIES[d as usize].members {
            let js = match object {
                None => Value::Undefined,
                Some(o) => {
                    let key = agent.heap.key_from_str(m.name);
                    ops::get(agent, o, key)?
                }
            };
            if !js.is_undefined() {
                let value = convert(agent, js, m.ty, Flags::default())?;
                members.push((m.name, value));
            } else if let Some(default) = m.default {
                let value = default_value(agent, default, m.ty)?;
                members.push((m.name, value));
            } else if m.required {
                return agent.type_error(&format!(
                    "The required member '{}' of '{}' is undefined.",
                    m.name, DICTIONARIES[d as usize].name
                ));
            }
        }
    }
    Ok(IdlValue::Dictionary(Box::new(Dict { id, members })))
}

/// Creating a sequence from an iterable (Web IDL §3.2.18).
fn sequence(agent: &mut Agent, v: Value, elem: Ty, method: Value) -> JsResult<IdlValue> {
    let mut record = ops::get_iterator_from_method(agent, v, method)?;
    let mut out = Vec::new();
    while let Some(next) = ops::iterator_step_value(agent, &mut record)? {
        out.push(convert(agent, next, elem, Flags::default())?);
    }
    Ok(IdlValue::Sequence(out))
}

/// Converting to a record (Web IDL §3.2.20).
fn record(agent: &mut Agent, v: Value, key_ty: Ty, value_ty: Ty) -> JsResult<IdlValue> {
    let Value::Object(o) = v else {
        return agent.type_error("The value is not an object.");
    };
    let keys = internal::own_property_keys(agent, o)?;
    let mut out: Vec<(String, IdlValue)> = Vec::new();
    for key in keys {
        let desc = internal::get_own_property(agent, o, key)?;
        if !desc.is_some_and(|d| d.enumerable == Some(true)) {
            continue;
        }
        let key_value = agent.heap.key_to_value(key);
        let typed_key = match convert(agent, key_value, key_ty, Flags::default())? {
            IdlValue::String(s) => s,
            _ => String::new(),
        };
        let js = ops::get(agent, o, key)?;
        let typed = convert(agent, js, value_ty, Flags::default())?;
        match out.iter_mut().find(|(k, _)| *k == typed_key) {
            Some(entry) => entry.1 = typed,
            None => out.push((typed_key, typed)),
        }
    }
    Ok(IdlValue::Record(out))
}

fn typed_array_kind(kind: TypedArrayKind) -> BufferKind {
    match kind {
        TypedArrayKind::Int8 => BufferKind::Int8Array,
        TypedArrayKind::Uint8 => BufferKind::Uint8Array,
        TypedArrayKind::Uint8Clamped => BufferKind::Uint8ClampedArray,
        TypedArrayKind::Int16 => BufferKind::Int16Array,
        TypedArrayKind::Uint16 => BufferKind::Uint16Array,
        TypedArrayKind::Int32 => BufferKind::Int32Array,
        TypedArrayKind::Uint32 => BufferKind::Uint32Array,
        TypedArrayKind::Float16 => BufferKind::Float16Array,
        TypedArrayKind::Float32 => BufferKind::Float32Array,
        TypedArrayKind::Float64 => BufferKind::Float64Array,
        TypedArrayKind::BigInt64 => BufferKind::BigInt64Array,
        TypedArrayKind::BigUint64 => BufferKind::BigUint64Array,
    }
}

/// True if `o` is a buffer source object of `kind`.
fn buffer_matches(agent: &Agent, o: Object, kind: BufferKind, allow_shared: bool) -> bool {
    match &agent.heap.object(o).kind {
        ObjectKind::ArrayBuffer(b) => match kind {
            BufferKind::ArrayBuffer => !b.is_shared(),
            BufferKind::SharedArrayBuffer => b.is_shared(),
            _ => false,
        },
        ObjectKind::DataView(_) => kind == BufferKind::DataView,
        ObjectKind::TypedArray(t) => {
            typed_array_kind(t.kind) == kind
                && (allow_shared || !vjs::buffer::is_shared(agent, t.buffer))
        }
        _ => false,
    }
}

/// The flattened member types of a union, without nullable wrappers
/// (Web IDL §2.13.24). Types defined outside the copied IDL files are
/// left out when other members exist.
fn flatten(members: &[Ty], out: &mut Vec<Ty>) {
    for &t in members {
        match t.inner() {
            Ty::Union(m) => flatten(m, out),
            Ty::Nullable(i) => flatten(std::slice::from_ref(i), out),
            _ => out.push(t),
        }
    }
}

fn is_numeric(t: Ty) -> bool {
    matches!(
        t.inner(),
        Ty::Byte
            | Ty::Octet
            | Ty::Short
            | Ty::UnsignedShort
            | Ty::Long
            | Ty::UnsignedLong
            | Ty::LongLong
            | Ty::UnsignedLongLong
            | Ty::Float
            | Ty::UnrestrictedFloat
            | Ty::Double
            | Ty::UnrestrictedDouble
    )
}

fn is_string(t: Ty) -> bool {
    matches!(
        t.inner(),
        Ty::DomString | Ty::ByteString | Ty::UsvString | Ty::Enum(_)
    )
}

/// ES-to-IDL union conversion (Web IDL §3.2.22).
fn union(agent: &mut Agent, v: Value, members: &[Ty], f: Flags) -> JsResult<IdlValue> {
    let mut all = Vec::new();
    flatten(members, &mut all);
    let known: Vec<Ty> = all
        .iter()
        .copied()
        .filter(|t| !matches!(t.inner(), Ty::Unknown(_)))
        .collect();
    let types = if known.is_empty() { all } else { known };
    let find = |pred: &dyn Fn(Ty) -> bool| types.iter().copied().find(|t| pred(*t));

    if v.is_undefined() {
        if find(&|t| t.inner() == Ty::Undefined).is_some() {
            return Ok(IdlValue::Undefined);
        }
    }
    if v.is_nullish() && members.iter().any(|m| m.includes_nullable()) {
        return Ok(IdlValue::Null);
    }
    if v.is_nullish() {
        if let Some(d) = find(&|t| matches!(t.inner(), Ty::Dictionary(_))) {
            return convert(agent, v, d, f);
        }
    }
    let object_member = find(&|t| t.inner() == Ty::Object);
    if let Some(p) = platform_ref(agent, v) {
        let iface = p.get().iface;
        let found = types.iter().copied().find(|t| match t.inner() {
            Ty::Interface(id) => super::platform::implements(iface, id),
            _ => false,
        });
        if found.is_some() || object_member.is_some() {
            return Ok(IdlValue::Object(p.object));
        }
    }
    if let Value::Object(o) = v {
        for t in &types {
            let shared = f.allow_shared || matches!(t, Ty::AllowShared(_));
            if let Ty::Buffer(k) = t.inner() {
                if buffer_matches(agent, o, k, shared) {
                    return Ok(IdlValue::Object(o));
                }
            }
        }
    }
    if is_callable(agent, v) {
        if let Some(c) = find(&|t| matches!(t.inner(), Ty::Callback(_))) {
            return convert(agent, v, c, f);
        }
        if object_member.is_some() {
            return Ok(IdlValue::Object(v.as_object().expect("callable")));
        }
    }
    if let Value::Object(o) = v {
        if let Some(s) = find(&|t| {
            matches!(
                t.inner(),
                Ty::Sequence(_) | Ty::FrozenArray(_) | Ty::ObservableArray(_)
            )
        }) {
            let key = PropertyKey::Symbol(agent.symbols.iterator);
            if let Some(method) = ops::get_method(agent, v, key)? {
                let elem = match s.inner() {
                    Ty::Sequence(e) | Ty::FrozenArray(e) | Ty::ObservableArray(e) => *e,
                    _ => Ty::Any,
                };
                return sequence(agent, v, elem, method);
            }
        }
        if let Some(d) = find(&|t| matches!(t.inner(), Ty::Dictionary(_))) {
            return convert(agent, v, d, f);
        }
        if let Some(r) = find(&|t| matches!(t.inner(), Ty::Record(..))) {
            return convert(agent, v, r, f);
        }
        if find(&|t| matches!(t.inner(), Ty::CallbackInterface(_))).is_some()
            || object_member.is_some()
        {
            return Ok(IdlValue::Object(o));
        }
    }
    if let Value::Boolean(b) = v {
        if find(&|t| t.inner() == Ty::Boolean).is_some() {
            return Ok(IdlValue::Bool(b));
        }
    }
    if let Value::Number(_) = v {
        if let Some(n) = find(&is_numeric) {
            return convert(agent, v, n, f);
        }
    }
    if let Value::BigInt(_) = v {
        if find(&|t| t.inner() == Ty::BigInt).is_some() {
            return Ok(IdlValue::BigInt(v));
        }
    }
    if let Value::Symbol(_) = v {
        if find(&|t| t.inner() == Ty::Symbol).is_some() {
            return Ok(IdlValue::Any(v));
        }
    }
    if let Some(s) = find(&is_string) {
        return convert(agent, v, s, f);
    }
    let numeric = find(&is_numeric);
    let bigint = find(&|t| t.inner() == Ty::BigInt);
    if let (Some(n), Some(_)) = (numeric, bigint) {
        return match ops::to_numeric(agent, v)? {
            Numeric::BigInt(b) => Ok(IdlValue::BigInt(Value::BigInt(b))),
            Numeric::Number(x) => convert(agent, Value::Number(x), n, f),
        };
    }
    if let Some(n) = numeric {
        return convert(agent, v, n, f);
    }
    if let Some(b) = find(&|t| t.inner() == Ty::Boolean) {
        return convert(agent, v, b, f);
    }
    if let Some(b) = bigint {
        return convert(agent, v, b, f);
    }
    if let Some(a) = find(&|t| matches!(t.inner(), Ty::Any | Ty::Unknown(_))) {
        return convert(agent, v, a, f);
    }
    let names: Vec<String> = types.iter().map(|t| type_name(*t)).collect();
    agent.type_error(&format!(
        "The value is not of type '({})'.",
        names.join(" or ")
    ))
}

// ----- IDL to ECMAScript --------------------------------------------------------

/// The element type of a sequence type (through nullable and unions).
fn element_type(ty: Ty) -> Ty {
    match ty.inner() {
        Ty::Sequence(t) | Ty::FrozenArray(t) | Ty::ObservableArray(t) => *t,
        Ty::Nullable(t) => element_type(*t),
        Ty::Union(members) => members
            .iter()
            .map(|m| element_type(*m))
            .find(|t| *t != Ty::Any)
            .unwrap_or(Ty::Any),
        _ => Ty::Any,
    }
}

fn is_frozen(ty: Ty) -> bool {
    match ty.inner() {
        Ty::FrozenArray(_) => true,
        Ty::Nullable(t) => is_frozen(*t),
        _ => false,
    }
}

fn record_value_type(ty: Ty) -> Ty {
    match ty.inner() {
        Ty::Record(_, v) => *v,
        Ty::Nullable(t) => record_value_type(*t),
        Ty::Union(members) => members
            .iter()
            .map(|m| record_value_type(*m))
            .find(|t| *t != Ty::Any)
            .unwrap_or(Ty::Any),
        _ => Ty::Any,
    }
}

/// Converts an IDL value of type `ty` to an ECMAScript value (Web IDL
/// §3.2).
pub fn to_js(agent: &mut Agent, v: IdlValue, ty: Ty) -> JsResult<Value> {
    Ok(match v {
        IdlValue::Missing | IdlValue::Undefined => Value::Undefined,
        IdlValue::Null => Value::Null,
        IdlValue::Bool(b) => Value::Boolean(b),
        IdlValue::Int(i) => Value::Number(i as f64),
        IdlValue::UInt(u) => Value::Number(u as f64),
        IdlValue::Double(d) => Value::Number(d),
        IdlValue::BigInt(v) | IdlValue::Any(v) => v,
        IdlValue::String(s) => Value::String(agent.heap.alloc_str(&s)),
        IdlValue::Object(o) => Value::Object(o),
        IdlValue::Sequence(items) => {
            let elem = element_type(ty);
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(to_js(agent, item, elem)?);
            }
            let array = ops::create_array_from_list(agent, &values);
            if is_frozen(ty) {
                ops::set_integrity_level(agent, array, IntegrityLevel::Frozen)?;
            }
            Value::Object(array)
        }
        IdlValue::Record(entries) => {
            let value_ty = record_value_type(ty);
            let o = ops::object_create(agent);
            for (k, item) in entries {
                let value = to_js(agent, item, value_ty)?;
                let key = agent.heap.key_from_str(&k);
                ops::create_data_property_or_throw(agent, o, key, value)?;
            }
            Value::Object(o)
        }
        IdlValue::Dictionary(d) => dictionary_to_js(agent, *d)?,
    })
}

/// A dictionary as an ordinary object (Web IDL §3.2.17).
pub fn dictionary_to_js(agent: &mut Agent, d: Dict) -> JsResult<Value> {
    let o = ops::object_create(agent);
    let chain = dictionary_chain(d.id);
    for (name, item) in d.members {
        let ty = chain
            .iter()
            .flat_map(|c| DICTIONARIES[*c as usize].members.iter())
            .find(|m| m.name == name)
            .map_or(Ty::Any, |m| m.ty);
        let value = to_js(agent, item, ty)?;
        let key = agent.heap.key_from_str(name);
        ops::create_data_property_or_throw(agent, o, key, value)?;
    }
    Ok(Value::Object(o))
}

/// An IDL value of no particular type as an ECMAScript value.
pub fn to_js_any(agent: &mut Agent, v: &IdlValue) -> JsResult<Value> {
    to_js(agent, v.clone(), Ty::Any)
}
