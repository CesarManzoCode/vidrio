//! The effective overload set and the overload resolution algorithm (Web
//! IDL §2.5.8 and §3.6).

use vjs::function::is_callable;
use vjs::object::ObjectKind;
use vjs::ops;
use vjs::{Agent, JsResult, PropertyKey, Value};

use super::convert::{default_value, to_idl};
use super::meta::{Operation, Overload, Ty};
use super::platform::{implements, platform_ref};
use super::value::IdlValue;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Optionality {
    Required,
    Optional,
    Variadic,
}

/// An entry of an effective overload set: the overload and its type list
/// (each type with its optionality and the declared argument it comes
/// from).
#[derive(Clone, Debug)]
struct Entry {
    overload: &'static Overload,
    items: Vec<(Ty, Optionality, usize)>,
}

/// The overload chosen by [`resolve`] and its converted arguments: every
/// declared argument (missing optional ones as their default or
/// [`IdlValue::Missing`]), then the extra values of a variadic argument.
#[derive(Debug)]
pub struct Resolved {
    pub overload: &'static Overload,
    pub args: Vec<IdlValue>,
}

/// The effective overload set of `op` for `n` arguments.
fn effective_set(op: &'static Operation, n: usize) -> Vec<Entry> {
    let maxarg = op
        .overloads
        .iter()
        .map(|o| o.args.len())
        .max()
        .unwrap_or(0)
        .max(n);
    let mut out = Vec::new();
    for ov in op.overloads {
        let items: Vec<(Ty, Optionality, usize)> = ov
            .args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let o = if a.variadic {
                    Optionality::Variadic
                } else if a.optional {
                    Optionality::Optional
                } else {
                    Optionality::Required
                };
                (a.ty, o, i)
            })
            .collect();
        out.push(Entry {
            overload: ov,
            items: items.clone(),
        });
        if let Some(&last) = items.last().filter(|l| l.1 == Optionality::Variadic) {
            let mut t = items.clone();
            for _ in ov.args.len()..maxarg {
                t.push(last);
                out.push(Entry {
                    overload: ov,
                    items: t.clone(),
                });
            }
        }
        let mut i = ov.args.len();
        while i > 0 {
            if ov.args[i - 1].optional || ov.args[i - 1].variadic {
                out.push(Entry {
                    overload: ov,
                    items: items[..i - 1].to_vec(),
                });
                i -= 1;
            } else {
                break;
            }
        }
    }
    out
}

/// True if `t` (through nullable types and unions) has a member type
/// for which `pred` holds.
fn has(t: Ty, pred: &dyn Fn(Ty) -> bool) -> bool {
    match t.inner() {
        Ty::Nullable(i) => has(*i, pred),
        Ty::Union(m) => m.iter().any(|x| has(*x, pred)),
        x => pred(x),
    }
}

fn is_numeric(t: Ty) -> bool {
    matches!(
        t,
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

/// Keeps the first entry whose type at `d` satisfies `pred`.
fn pick(s: &[Entry], d: usize, pred: &dyn Fn(Ty) -> bool) -> Option<Entry> {
    s.iter().find(|e| has(e.items[d].0, pred)).cloned()
}

/// Step 12 of the overload resolution algorithm: narrows the set by the
/// value at the distinguishing argument index `d`.
fn select(agent: &mut Agent, s: &[Entry], d: usize, v: Value) -> JsResult<Entry> {
    if v.is_undefined() {
        if let Some(e) = s.iter().find(|e| e.items[d].1 == Optionality::Optional) {
            return Ok(e.clone());
        }
        if let Some(e) = pick(s, d, &|t| t == Ty::Undefined) {
            return Ok(e);
        }
    }
    if v.is_nullish() {
        if let Some(e) = s.iter().find(|e| {
            let t = e.items[d].0;
            t.includes_nullable() || has(t, &|x| matches!(x, Ty::Dictionary(_)))
        }) {
            return Ok(e.clone());
        }
    }
    if let Some(p) = platform_ref(agent, v) {
        let iface = p.get().iface;
        if let Some(e) = pick(s, d, &|t| match t {
            Ty::Interface(id) => implements(iface, id),
            Ty::Object => true,
            _ => false,
        }) {
            return Ok(e);
        }
    }
    if let Value::Object(o) = v {
        let buffer = matches!(
            agent.heap.object(o).kind,
            ObjectKind::ArrayBuffer(_) | ObjectKind::DataView(_) | ObjectKind::TypedArray(_)
        );
        if buffer {
            if let Some(e) = pick(s, d, &|t| matches!(t, Ty::Buffer(_) | Ty::Object)) {
                return Ok(e);
            }
        }
    }
    if is_callable(agent, v) {
        if let Some(e) = pick(s, d, &|t| matches!(t, Ty::Callback(_) | Ty::Object)) {
            return Ok(e);
        }
    }
    if v.is_object() {
        if let Some(e) = pick(s, d, &|t| {
            matches!(
                t,
                Ty::Sequence(_) | Ty::FrozenArray(_) | Ty::ObservableArray(_)
            )
        }) {
            let key = PropertyKey::Symbol(agent.symbols.iterator);
            if ops::get_method(agent, v, key)?.is_some() {
                return Ok(e);
            }
        }
        if let Some(e) = pick(s, d, &|t| {
            matches!(
                t,
                Ty::CallbackInterface(_) | Ty::Dictionary(_) | Ty::Record(..) | Ty::Object
            )
        }) {
            return Ok(e);
        }
    }
    let by_value: Option<Entry> = match v {
        Value::Boolean(_) => pick(s, d, &|t| t == Ty::Boolean),
        Value::Number(_) => pick(s, d, &is_numeric),
        Value::BigInt(_) => pick(s, d, &|t| t == Ty::BigInt),
        _ => None,
    };
    if let Some(e) = by_value {
        return Ok(e);
    }
    let fallbacks: [&dyn Fn(Ty) -> bool; 5] = [
        &|t| {
            matches!(
                t,
                Ty::DomString | Ty::ByteString | Ty::UsvString | Ty::Enum(_)
            )
        },
        &is_numeric,
        &|t| t == Ty::BigInt,
        &|t| t == Ty::Boolean,
        &|t| matches!(t, Ty::Any | Ty::Unknown(_)),
    ];
    for pred in fallbacks {
        if let Some(e) = pick(s, d, pred) {
            return Ok(e);
        }
    }
    agent.type_error("No overload matches the arguments.")
}

/// The distinguishing argument index of a set whose entries have the
/// same length: the first position whose types differ.
fn distinguishing_index(s: &[Entry]) -> Option<usize> {
    let len = s.first()?.items.len();
    (0..len).find(|&i| s.iter().any(|e| e.items[i].0 != s[0].items[i].0))
}

fn convert_arg(agent: &mut Agent, v: Value, entry: &Entry, i: usize) -> JsResult<IdlValue> {
    let (ty, optionality, declared) = entry.items[i];
    if optionality == Optionality::Optional && v.is_undefined() {
        return match entry.overload.args[declared].default {
            Some(d) => default_value(agent, d, ty),
            None => Ok(IdlValue::Missing),
        };
    }
    to_idl(agent, v, ty)
}

/// The overload resolution algorithm (Web IDL §3.6): chooses an overload
/// of `op` for `args` and converts them.
pub fn resolve(agent: &mut Agent, op: &'static Operation, args: &[Value]) -> JsResult<Resolved> {
    let n = args.len();
    let set = effective_set(op, n);
    let maxarg = set.iter().map(|e| e.items.len()).max().unwrap_or(0);
    let argcount = maxarg.min(n);
    let s: Vec<Entry> = set
        .into_iter()
        .filter(|e| e.items.len() == argcount)
        .collect();
    if s.is_empty() {
        return agent.type_error(&format!(
            "Failed to execute '{}': {} argument{} required, but only {n} present.",
            op.name,
            op.length,
            if op.length == 1 { "" } else { "s" }
        ));
    }
    let mut values = Vec::new();
    let mut i = 0;
    let entry = match (s.len() > 1).then(|| distinguishing_index(&s)).flatten() {
        Some(d) => {
            while i < d {
                values.push(convert_arg(agent, args[i], &s[0], i)?);
                i += 1;
            }
            select(agent, &s, d, args[d])?
        }
        None => s[0].clone(),
    };
    while i < argcount {
        values.push(convert_arg(agent, args[i], &entry, i)?);
        i += 1;
    }
    let declared = entry.overload.args.len();
    while i < declared {
        let a = &entry.overload.args[i];
        if a.variadic {
            break;
        }
        values.push(match a.default {
            Some(d) => default_value(agent, d, a.ty)?,
            None => IdlValue::Missing,
        });
        i += 1;
    }
    Ok(Resolved {
        overload: entry.overload,
        args: values,
    })
}
