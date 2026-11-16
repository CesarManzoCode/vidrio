//! `String` (ECMA-262 §22.1): the constructor, `fromCharCode`,
//! `fromCodePoint` and `raw`, every method of `String.prototype` including
//! the HTML methods of Annex B.2.2.2–14, and `%StringIteratorPrototype%`. String exotic
//! objects themselves are in `crate::exotic` and `crate::internal`.
//!
//! Case mapping and normalization use the Unicode 17.0.0 tables of
//! [`crate::unicode`]; there is no locale data, so the `toLocale…` methods
//! map like their locale-free counterparts and `localeCompare` compares
//! the code units of the NFC forms.

use crate::agent::Agent;
use crate::builtins::symbol::symbol_descriptive_string;
use crate::builtins::{constructor, intrinsic, method, set_intrinsic, to_string_tag};
use crate::completion::JsResult;
use crate::function::{NativeCall, create_builtin_function, is_callable};
use crate::gc::{JsString, Object, Realm, Trace, Tracer};
use crate::internal;
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::string::MAX_STRING_LENGTH;
use crate::unicode::{self, NormalizationForm};
use crate::value::{PropertyKey, Value};

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let proto = intrinsic(agent, realm, Intrinsic::StringPrototype);
    let ctor = constructor(agent, realm, "String", string_constructor, 1, proto, None);
    set_intrinsic(agent, realm, Intrinsic::String, ctor);
    method(agent, realm, ctor, "fromCharCode", from_char_code, 1);
    method(agent, realm, ctor, "fromCodePoint", from_code_point, 1);
    method(agent, realm, ctor, "raw", raw, 1);

    let methods: &[(&str, crate::function::NativeFn, u32)] = &[
        ("at", at, 1),
        ("charAt", char_at, 1),
        ("charCodeAt", char_code_at, 1),
        ("codePointAt", code_point_at, 1),
        ("concat", concat, 1),
        ("endsWith", ends_with, 1),
        ("includes", includes, 1),
        ("indexOf", index_of, 1),
        ("isWellFormed", is_well_formed, 0),
        ("lastIndexOf", last_index_of, 1),
        ("localeCompare", locale_compare, 1),
        ("match", match_, 1),
        ("matchAll", match_all, 1),
        ("normalize", normalize, 0),
        ("padEnd", pad_end, 1),
        ("padStart", pad_start, 1),
        ("repeat", repeat, 1),
        ("replace", replace, 2),
        ("replaceAll", replace_all, 2),
        ("search", search, 1),
        ("slice", slice, 2),
        ("split", split, 2),
        ("startsWith", starts_with, 1),
        ("substr", substr, 2),
        ("substring", substring, 2),
        ("toLocaleLowerCase", to_lower_case, 0),
        ("toLocaleUpperCase", to_upper_case, 0),
        ("toLowerCase", to_lower_case, 0),
        ("toString", to_string, 0),
        ("toUpperCase", to_upper_case, 0),
        ("toWellFormed", to_well_formed, 0),
        ("trim", trim, 0),
        ("valueOf", to_string, 0),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    for &(name, f, length) in HTML_METHODS {
        method(agent, realm, proto, name, f, length);
    }
    // Annex B: trimLeft and trimRight are the same function objects as
    // trimStart and trimEnd.
    let trim_start = method(agent, realm, proto, "trimStart", trim_start, 0);
    let trim_end = method(agent, realm, proto, "trimEnd", trim_end, 0);
    for (name, f) in [("trimLeft", trim_start), ("trimRight", trim_end)] {
        let key = agent.heap.key_from_str(name);
        ops::define_builtin_method(agent, proto, key, f);
    }
    let iterator_key = PropertyKey::Symbol(agent.symbols.iterator);
    let iterator = create_builtin_function(
        agent,
        string_iterator,
        0,
        iterator_key,
        Some(realm),
        None,
        None,
    );
    ops::define_builtin_method(agent, proto, iterator_key, iterator);

    let iterator_proto = intrinsic(agent, realm, Intrinsic::IteratorPrototype);
    let string_iterator_proto = agent
        .heap
        .create_object(Some(iterator_proto), ObjectKind::Ordinary);
    method(
        agent,
        realm,
        string_iterator_proto,
        "next",
        string_iterator_next,
        0,
    );
    to_string_tag(agent, string_iterator_proto, "String Iterator");
    set_intrinsic(
        agent,
        realm,
        Intrinsic::StringIteratorPrototype,
        string_iterator_proto,
    );
}

// ----- helpers -----

/// A string value from code units, or a RangeError past
/// [`MAX_STRING_LENGTH`].
fn string_value(agent: &mut Agent, units: Vec<u16>) -> JsResult<Value> {
    if units.len() > MAX_STRING_LENGTH {
        return agent.range_error("string too long");
    }
    Ok(Value::String(agent.heap.alloc_string_owned(units)))
}

/// `? ToString(? RequireObjectCoercible(this value))`.
fn this_string(agent: &mut Agent, this: Value) -> JsResult<JsString> {
    let o = ops::require_object_coercible(agent, this)?;
    ops::to_string(agent, o)
}

fn units_of(agent: &mut Agent, s: JsString) -> Vec<u16> {
    agent.heap.string_units(s).to_vec()
}

fn this_units(agent: &mut Agent, this: Value) -> JsResult<Vec<u16>> {
    let s = this_string(agent, this)?;
    Ok(units_of(agent, s))
}

fn to_units(agent: &mut Agent, v: Value) -> JsResult<Vec<u16>> {
    let s = ops::to_string(agent, v)?;
    Ok(units_of(agent, s))
}

/// An integer clamped to `0..=len`.
fn clamp(n: f64, len: usize) -> usize {
    n.max(0.0).min(len as f64) as usize
}

/// A relative index (negative counts from the end) clamped to `0..=len`,
/// as `slice` uses it.
fn relative(n: f64, len: usize) -> usize {
    if n < 0.0 {
        (len as f64 + n).max(0.0) as usize
    } else {
        n.min(len as f64) as usize
    }
}

/// StringIndexOf(string, searchValue, fromIndex).
pub(crate) fn string_index_of(string: &[u16], search: &[u16], from: usize) -> Option<usize> {
    let len = string.len();
    if search.is_empty() {
        return (from <= len).then_some(from);
    }
    if search.len() > len {
        return None;
    }
    (from..=len - search.len()).find(|&i| string[i..i + search.len()] == *search)
}

/// IsRegExp(argument).
pub(crate) fn is_regexp(agent: &mut Agent, v: Value) -> JsResult<bool> {
    let Value::Object(o) = v else {
        return Ok(false);
    };
    let key = PropertyKey::Symbol(agent.symbols.match_);
    let matcher = ops::get(agent, o, key)?;
    if !matcher.is_undefined() {
        return Ok(ops::to_boolean(agent, matcher));
    }
    Ok(has_regexp_matcher(agent, o))
}

/// Whether `o` has a `[[RegExpMatcher]]` internal slot.
fn has_regexp_matcher(agent: &Agent, o: Object) -> bool {
    crate::builtins::regexp::is_regexp_object(agent, o)
}

/// RegExpCreate(P, F) through `%RegExp%`: converting the pattern to a
/// string first makes `new %RegExp%(P, F)` take the same steps.
fn regexp_create(agent: &mut Agent, pattern: Value, flags: Option<&str>) -> JsResult<Object> {
    let realm = agent.current_realm();
    let Some(ctor) = agent
        .heap
        .realm(realm)
        .intrinsics
        .try_get(Intrinsic::RegExp)
    else {
        return agent.type_error("RegExp is not available");
    };
    let p = if pattern.is_undefined() {
        Value::String(agent.heap.empty_string())
    } else {
        Value::String(ops::to_string(agent, pattern)?)
    };
    let f = match flags {
        Some(f) => Value::String(agent.heap.alloc_str(f)),
        None => Value::Undefined,
    };
    ops::construct(agent, ctor, &[p, f], None)
}

/// The RegExp checks of `matchAll` and `replaceAll`: a regular expression
/// argument must have the `g` flag.
fn require_global_flag(agent: &mut Agent, v: Value) -> JsResult<()> {
    if !is_regexp(agent, v)? {
        return Ok(());
    }
    let key = agent.heap.key_from_str("flags");
    let flags = ops::get_v(agent, v, key)?;
    ops::require_object_coercible(agent, flags)?;
    let flags = to_units(agent, flags)?;
    if !flags.contains(&u16::from(b'g')) {
        return agent.type_error("the RegExp argument must have the global flag");
    }
    Ok(())
}

/// Calls `value[symbol](this, ...args)` when `value` is an object with
/// that method (match, matchAll, replace, search and split).
fn delegate(
    agent: &mut Agent,
    value: Value,
    symbol: crate::gc::Symbol,
    args: &[Value],
) -> JsResult<Option<Value>> {
    if !matches!(value, Value::Object(_)) {
        return Ok(None);
    }
    match ops::get_method(agent, value, PropertyKey::Symbol(symbol))? {
        Some(f) => Ok(Some(ops::call(agent, f, value, args)?)),
        None => Ok(None),
    }
}

// ----- constructor and statics -----

/// `String(value)` (§22.1.1.1).
fn string_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = if call.args.is_empty() {
        agent.heap.empty_string()
    } else {
        let value = call.arg(0);
        if call.new_target.is_none()
            && let Value::Symbol(sym) = value
        {
            return Ok(symbol_descriptive_string(agent, sym));
        }
        ops::to_string(agent, value)?
    };
    let Some(new_target) = call.new_target else {
        return Ok(Value::String(s));
    };
    let proto = ops::get_prototype_from_constructor(agent, new_target, Intrinsic::StringPrototype)?;
    Ok(Value::Object(internal::string_create(
        agent,
        s,
        Some(proto),
    )))
}

/// `String.fromCharCode(...codeUnits)`.
fn from_char_code(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let mut units = Vec::with_capacity(call.args.len());
    for &v in call.args {
        units.push(ops::to_uint16(agent, v)?);
    }
    string_value(agent, units)
}

/// `String.fromCodePoint(...codePoints)`.
fn from_code_point(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let mut units = Vec::with_capacity(call.args.len());
    for &v in call.args {
        let n = ops::to_number(agent, v)?;
        if !(n.is_finite() && n.trunc() == n && (0.0..=1114111.0).contains(&n)) {
            return agent.range_error("invalid code point");
        }
        unicode::push_utf16(&mut units, n as u32);
    }
    string_value(agent, units)
}

/// `String.raw(template, ...substitutions)`.
fn raw(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let substitutions = call.args.get(1..).unwrap_or(&[]);
    let cooked = ops::to_object(agent, call.arg(0))?;
    let key = agent.heap.key_from_str("raw");
    let raw = ops::get(agent, cooked, key)?;
    let literals = ops::to_object(agent, raw)?;
    let count = ops::length_of_array_like(agent, literals)?;
    let mut out = Vec::new();
    if count <= 0.0 {
        return Ok(Value::String(agent.heap.empty_string()));
    }
    let mut i = 0u64;
    loop {
        let key = ops::index_key(agent, i);
        let literal = ops::get(agent, literals, key)?;
        out.extend(to_units(agent, literal)?);
        if out.len() > MAX_STRING_LENGTH {
            return agent.range_error("string too long");
        }
        if (i + 1) as f64 == count {
            return string_value(agent, out);
        }
        if let Some(&sub) = substitutions.get(i as usize) {
            out.extend(to_units(agent, sub)?);
        }
        i += 1;
    }
}

// ----- String.prototype -----

/// thisStringValue(value).
fn this_string_value(agent: &mut Agent, v: Value) -> JsResult<JsString> {
    match v {
        Value::String(s) => Ok(s),
        Value::Object(o) => {
            let inner = match &agent.heap.object(o).kind {
                ObjectKind::String(s) => Some(*s),
                _ => None,
            };
            match inner {
                Some(s) => Ok(s),
                None => agent.type_error("not a String object"),
            }
        }
        _ => agent.type_error("not a string"),
    }
}

/// `toString()` and `valueOf()`.
fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::String(this_string_value(agent, call.this)?))
}

fn at(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let len = agent.heap.string_len(s);
    let rel = ops::to_integer_or_infinity(agent, call.arg(0))?;
    let k = if rel >= 0.0 { rel } else { len as f64 + rel };
    if k < 0.0 || k >= len as f64 {
        return Ok(Value::Undefined);
    }
    let k = k as usize;
    Ok(Value::String(agent.heap.substring(s, k, k + 1)))
}

fn char_at(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let len = agent.heap.string_len(s);
    let pos = ops::to_integer_or_infinity(agent, call.arg(0))?;
    if pos < 0.0 || pos >= len as f64 {
        return Ok(Value::String(agent.heap.empty_string()));
    }
    let p = pos as usize;
    Ok(Value::String(agent.heap.substring(s, p, p + 1)))
}

fn char_code_at(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let len = agent.heap.string_len(s);
    let pos = ops::to_integer_or_infinity(agent, call.arg(0))?;
    if pos < 0.0 || pos >= len as f64 {
        return Ok(Value::Number(f64::NAN));
    }
    let unit = agent.heap.code_unit_at(s, pos as usize).unwrap_or(0);
    Ok(Value::Number(f64::from(unit)))
}

fn code_point_at(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let len = agent.heap.string_len(s);
    let pos = ops::to_integer_or_infinity(agent, call.arg(0))?;
    if pos < 0.0 || pos >= len as f64 {
        return Ok(Value::Undefined);
    }
    let (cp, _) = unicode::code_point_at(agent.heap.string_units(s), pos as usize);
    Ok(Value::Number(f64::from(cp)))
}

fn concat(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let mut r = this_string(agent, call.this)?;
    for &v in call.args {
        let next = ops::to_string(agent, v)?;
        match agent.heap.concat(r, next) {
            Some(joined) => r = joined,
            None => return agent.range_error("string too long"),
        }
    }
    Ok(Value::String(r))
}

/// The shared steps of `startsWith`, `endsWith` and `includes`: the
/// string, the search string (a RegExp argument is a TypeError).
fn search_args(agent: &mut Agent, call: &NativeCall<'_>) -> JsResult<(Vec<u16>, Vec<u16>)> {
    let s = this_units(agent, call.this)?;
    if is_regexp(agent, call.arg(0))? {
        return agent.type_error("the search string must not be a RegExp");
    }
    let search = to_units(agent, call.arg(0))?;
    Ok((s, search))
}

fn ends_with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (s, search) = search_args(agent, &call)?;
    let len = s.len();
    let end = match call.arg(1) {
        Value::Undefined => len,
        v => clamp(ops::to_integer_or_infinity(agent, v)?, len),
    };
    if search.is_empty() {
        return Ok(Value::Boolean(true));
    }
    let Some(start) = end.checked_sub(search.len()) else {
        return Ok(Value::Boolean(false));
    };
    Ok(Value::Boolean(s[start..end] == *search))
}

fn starts_with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (s, search) = search_args(agent, &call)?;
    let len = s.len();
    let start = clamp(ops::to_integer_or_infinity(agent, call.arg(1))?, len);
    if search.is_empty() {
        return Ok(Value::Boolean(true));
    }
    let end = start + search.len();
    if end > len {
        return Ok(Value::Boolean(false));
    }
    Ok(Value::Boolean(s[start..end] == *search))
}

fn includes(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (s, search) = search_args(agent, &call)?;
    let start = clamp(ops::to_integer_or_infinity(agent, call.arg(1))?, s.len());
    Ok(Value::Boolean(
        string_index_of(&s, &search, start).is_some(),
    ))
}

fn index_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_units(agent, call.this)?;
    let search = to_units(agent, call.arg(0))?;
    let start = clamp(ops::to_integer_or_infinity(agent, call.arg(1))?, s.len());
    Ok(Value::Number(
        string_index_of(&s, &search, start).map_or(-1.0, |i| i as f64),
    ))
}

fn last_index_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_units(agent, call.this)?;
    let search = to_units(agent, call.arg(0))?;
    let num_pos = ops::to_number(agent, call.arg(1))?;
    let pos = if num_pos.is_nan() {
        f64::INFINITY
    } else {
        ops::f64_to_integer_or_infinity(num_pos)
    };
    let len = s.len();
    let start = clamp(pos, len);
    let Some(last) = len.checked_sub(search.len()) else {
        return Ok(Value::Number(-1.0));
    };
    let found = (0..=start.min(last))
        .rev()
        .find(|&i| s[i..i + search.len()] == *search);
    Ok(Value::Number(found.map_or(-1.0, |i| i as f64)))
}

fn is_well_formed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_units(agent, call.this)?;
    Ok(Value::Boolean(char::decode_utf16(s).all(|c| c.is_ok())))
}

fn to_well_formed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let units = units_of(agent, s);
    if char::decode_utf16(units.iter().copied()).all(|c| c.is_ok()) {
        return Ok(Value::String(s));
    }
    let mut out = Vec::with_capacity(units.len());
    for c in char::decode_utf16(units) {
        let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
        unicode::push_utf16(&mut out, c as u32);
    }
    string_value(agent, out)
}

/// `localeCompare(that)`: without locale data, the code-unit order of the
/// NFC forms, so canonically equivalent strings compare equal.
fn locale_compare(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_units(agent, call.this)?;
    let that = to_units(agent, call.arg(0))?;
    let a = unicode::normalize_utf16(&s, NormalizationForm::Nfc);
    let b = unicode::normalize_utf16(&that, NormalizationForm::Nfc);
    Ok(Value::Number(match a.cmp(&b) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn match_(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::require_object_coercible(agent, call.this)?;
    let regexp = call.arg(0);
    let symbol = agent.symbols.match_;
    if let Some(r) = delegate(agent, regexp, symbol, &[o])? {
        return Ok(r);
    }
    let s = ops::to_string(agent, o)?;
    let rx = regexp_create(agent, regexp, None)?;
    ops::invoke(
        agent,
        Value::Object(rx),
        PropertyKey::Symbol(symbol),
        &[Value::String(s)],
    )
}

fn match_all(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::require_object_coercible(agent, call.this)?;
    let regexp = call.arg(0);
    let symbol = agent.symbols.match_all;
    if matches!(regexp, Value::Object(_)) {
        require_global_flag(agent, regexp)?;
    }
    if let Some(r) = delegate(agent, regexp, symbol, &[o])? {
        return Ok(r);
    }
    let s = ops::to_string(agent, o)?;
    let rx = regexp_create(agent, regexp, Some("g"))?;
    ops::invoke(
        agent,
        Value::Object(rx),
        PropertyKey::Symbol(symbol),
        &[Value::String(s)],
    )
}

fn search(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::require_object_coercible(agent, call.this)?;
    let regexp = call.arg(0);
    let symbol = agent.symbols.search;
    if let Some(r) = delegate(agent, regexp, symbol, &[o])? {
        return Ok(r);
    }
    let s = ops::to_string(agent, o)?;
    let rx = regexp_create(agent, regexp, None)?;
    ops::invoke(
        agent,
        Value::Object(rx),
        PropertyKey::Symbol(symbol),
        &[Value::String(s)],
    )
}

fn normalize(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let form = match call.arg(0) {
        Value::Undefined => NormalizationForm::Nfc,
        v => {
            let name = ops::to_string(agent, v)?;
            let name = agent.heap.string_to_rust(name);
            match NormalizationForm::from_name(&name) {
                Some(f) => f,
                None => return agent.range_error("invalid normalization form"),
            }
        }
    };
    let units = units_of(agent, s);
    let out = unicode::normalize_utf16(&units, form);
    if out == units {
        return Ok(Value::String(s));
    }
    string_value(agent, out)
}

/// StringPaddingBuiltinsImpl(O, maxLength, fillString, placement).
fn pad(agent: &mut Agent, call: NativeCall<'_>, at_start: bool) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let max = ops::to_length(agent, call.arg(0))?;
    let len = agent.heap.string_len(s);
    if max <= len as f64 {
        return Ok(Value::String(s));
    }
    let fill = match call.arg(1) {
        Value::Undefined => vec![u16::from(b' ')],
        v => to_units(agent, v)?,
    };
    if fill.is_empty() {
        return Ok(Value::String(s));
    }
    if max > MAX_STRING_LENGTH as f64 {
        return agent.range_error("string too long");
    }
    let fill_len = max as usize - len;
    let filler: Vec<u16> = fill.iter().copied().cycle().take(fill_len).collect();
    let units = units_of(agent, s);
    let out = if at_start {
        [filler, units].concat()
    } else {
        [units, filler].concat()
    };
    string_value(agent, out)
}

fn pad_start(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    pad(agent, call, true)
}

fn pad_end(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    pad(agent, call, false)
}

fn repeat(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let n = ops::to_integer_or_infinity(agent, call.arg(0))?;
    if n < 0.0 || n == f64::INFINITY {
        return agent.range_error("invalid repeat count");
    }
    let len = agent.heap.string_len(s);
    if n == 0.0 || len == 0 {
        return Ok(Value::String(agent.heap.empty_string()));
    }
    if n * len as f64 > MAX_STRING_LENGTH as f64 {
        return agent.range_error("string too long");
    }
    let units = units_of(agent, s).repeat(n as usize);
    string_value(agent, units)
}

/// GetSubstitution(matched, str, position, captures, namedCaptures,
/// replacementTemplate) (§22.1.3.19.1). `captures` holds strings or
/// `undefined`; `named_captures` is an object or `undefined`.
pub(crate) fn get_substitution(
    agent: &mut Agent,
    matched: &[u16],
    string: &[u16],
    position: usize,
    captures: &[Value],
    named_captures: Value,
    template: &[u16],
) -> JsResult<Vec<u16>> {
    let digit = |u: u16| (u16::from(b'0')..=u16::from(b'9')).contains(&u);
    let len = string.len();
    let mut result = Vec::new();
    let mut i = 0;
    while i < template.len() {
        let rest = &template[i..];
        if rest[0] != u16::from(b'$') || rest.len() == 1 {
            result.push(rest[0]);
            i += 1;
            continue;
        }
        let next = rest[1];
        if next == u16::from(b'$') {
            result.push(u16::from(b'$'));
            i += 2;
        } else if next == u16::from(b'`') {
            result.extend_from_slice(&string[..position.min(len)]);
            i += 2;
        } else if next == u16::from(b'&') {
            result.extend_from_slice(matched);
            i += 2;
        } else if next == u16::from(b'\'') {
            let tail = (position + matched.len()).min(len);
            result.extend_from_slice(&string[tail..]);
            i += 2;
        } else if digit(next) {
            let mut digit_count = if rest.len() > 2 && digit(rest[2]) {
                2
            } else {
                1
            };
            let value = |count: usize| {
                rest[1..1 + count]
                    .iter()
                    .fold(0usize, |n, &u| n * 10 + usize::from(u - u16::from(b'0')))
            };
            let mut index = value(digit_count);
            if index > captures.len() && digit_count == 2 {
                digit_count = 1;
                index = value(1);
            }
            let reference = &rest[..1 + digit_count];
            if (1..=captures.len()).contains(&index) {
                if let Value::String(c) = captures[index - 1] {
                    result.extend_from_slice(agent.heap.string_units(c));
                }
            } else {
                result.extend_from_slice(reference);
            }
            i += 1 + digit_count;
        } else if next == u16::from(b'<') {
            let gt = string_index_of(rest, &[u16::from(b'>')], 0);
            match gt {
                Some(gt) if !named_captures.is_undefined() => {
                    let name = &rest[2..gt];
                    let key = agent.heap.key_from_units(name);
                    let capture = ops::get_v(agent, named_captures, key)?;
                    if !capture.is_undefined() {
                        result.extend(to_units(agent, capture)?);
                    }
                    i += gt + 1;
                }
                _ => {
                    result.extend_from_slice(&rest[..2]);
                    i += 2;
                }
            }
        } else {
            result.push(u16::from(b'$'));
            i += 1;
        }
        if result.len() > MAX_STRING_LENGTH {
            return agent.range_error("string too long");
        }
    }
    Ok(result)
}

/// The replacement of one match of `replace` or `replaceAll` with a string
/// pattern.
fn replacement(
    agent: &mut Agent,
    replace_value: Value,
    template: Option<&[u16]>,
    search: &[u16],
    string: JsString,
    units: &[u16],
    position: usize,
) -> JsResult<Vec<u16>> {
    match template {
        Some(t) => get_substitution(agent, search, units, position, &[], Value::Undefined, t),
        None => {
            let matched = Value::String(agent.heap.alloc_string(search));
            let args = [
                matched,
                Value::Number(position as f64),
                Value::String(string),
            ];
            let r = ops::call(agent, replace_value, Value::Undefined, &args)?;
            to_units(agent, r)
        }
    }
}

fn replace(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::require_object_coercible(agent, call.this)?;
    let (search_value, replace_value) = (call.arg(0), call.arg(1));
    let symbol = agent.symbols.replace;
    if let Some(r) = delegate(agent, search_value, symbol, &[o, replace_value])? {
        return Ok(r);
    }
    let string = ops::to_string(agent, o)?;
    let search = to_units(agent, search_value)?;
    let template = if is_callable(agent, replace_value) {
        None
    } else {
        Some(to_units(agent, replace_value)?)
    };
    let units = units_of(agent, string);
    let Some(position) = string_index_of(&units, &search, 0) else {
        return Ok(Value::String(string));
    };
    let r = replacement(
        agent,
        replace_value,
        template.as_deref(),
        &search,
        string,
        &units,
        position,
    )?;
    let out = [
        &units[..position],
        &r[..],
        &units[position + search.len()..],
    ]
    .concat();
    string_value(agent, out)
}

fn replace_all(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::require_object_coercible(agent, call.this)?;
    let (search_value, replace_value) = (call.arg(0), call.arg(1));
    let symbol = agent.symbols.replace;
    if matches!(search_value, Value::Object(_)) {
        require_global_flag(agent, search_value)?;
    }
    if let Some(r) = delegate(agent, search_value, symbol, &[o, replace_value])? {
        return Ok(r);
    }
    let string = ops::to_string(agent, o)?;
    let search = to_units(agent, search_value)?;
    let template = if is_callable(agent, replace_value) {
        None
    } else {
        Some(to_units(agent, replace_value)?)
    };
    let units = units_of(agent, string);
    let advance = search.len().max(1);
    let mut positions = Vec::new();
    let mut position = string_index_of(&units, &search, 0);
    while let Some(p) = position {
        positions.push(p);
        position = string_index_of(&units, &search, p + advance);
    }
    let mut end_of_last = 0;
    let mut out = Vec::new();
    for p in positions {
        out.extend_from_slice(&units[end_of_last..p]);
        let r = replacement(
            agent,
            replace_value,
            template.as_deref(),
            &search,
            string,
            &units,
            p,
        )?;
        out.extend(r);
        if out.len() > MAX_STRING_LENGTH {
            return agent.range_error("string too long");
        }
        end_of_last = p + search.len();
    }
    if end_of_last < units.len() {
        out.extend_from_slice(&units[end_of_last..]);
    }
    string_value(agent, out)
}

fn slice(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let len = agent.heap.string_len(s);
    let from = relative(ops::to_integer_or_infinity(agent, call.arg(0))?, len);
    let to = match call.arg(1) {
        Value::Undefined => len,
        v => relative(ops::to_integer_or_infinity(agent, v)?, len),
    };
    if from >= to {
        return Ok(Value::String(agent.heap.empty_string()));
    }
    Ok(Value::String(agent.heap.substring(s, from, to)))
}

fn substring(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let len = agent.heap.string_len(s);
    let start = clamp(ops::to_integer_or_infinity(agent, call.arg(0))?, len);
    let end = match call.arg(1) {
        Value::Undefined => len,
        v => clamp(ops::to_integer_or_infinity(agent, v)?, len),
    };
    let (from, to) = (start.min(end), start.max(end));
    Ok(Value::String(agent.heap.substring(s, from, to)))
}

/// CreateHTML(string, tag, attribute, value) (§B.2.2.2.1).
fn create_html(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    tag: &str,
    attribute: &str,
) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let mut out: Vec<u16> = Vec::new();
    out.push(u16::from(b'<'));
    out.extend(tag.encode_utf16());
    if !attribute.is_empty() {
        let v = ops::to_string(agent, call.arg(0))?;
        out.push(u16::from(b' '));
        out.extend(attribute.encode_utf16());
        out.extend("=\"".encode_utf16());
        for &c in agent.heap.string_units(v) {
            if c == u16::from(b'"') {
                out.extend("&quot;".encode_utf16());
            } else {
                out.push(c);
            }
        }
        out.push(u16::from(b'"'));
    }
    out.push(u16::from(b'>'));
    out.extend_from_slice(agent.heap.string_units(s));
    out.extend("</".encode_utf16());
    out.extend(tag.encode_utf16());
    out.push(u16::from(b'>'));
    if out.len() > MAX_STRING_LENGTH {
        return agent.range_error("string too long");
    }
    Ok(Value::String(agent.heap.alloc_string_owned(out)))
}

macro_rules! html_methods {
    ($($fn_name:ident = $name:literal, $tag:literal, $attribute:literal, $length:literal;)*) => {
        $(
            fn $fn_name(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
                create_html(agent, &call, $tag, $attribute)
            }
        )*
        /// The HTML methods of `String.prototype` (§B.2.2.2–14).
        const HTML_METHODS: &[(&str, crate::function::NativeFn, u32)] = &[
            $(($name, $fn_name, $length),)*
        ];
    };
}

html_methods! {
    html_anchor = "anchor", "a", "name", 1;
    html_big = "big", "big", "", 0;
    html_blink = "blink", "blink", "", 0;
    html_bold = "bold", "b", "", 0;
    html_fixed = "fixed", "tt", "", 0;
    html_fontcolor = "fontcolor", "font", "color", 1;
    html_fontsize = "fontsize", "font", "size", 1;
    html_italics = "italics", "i", "", 0;
    html_link = "link", "a", "href", 1;
    html_small = "small", "small", "", 0;
    html_strike = "strike", "strike", "", 0;
    html_sub = "sub", "sub", "", 0;
    html_sup = "sup", "sup", "", 0;
}

/// `substr(start, length)` (Annex B.2.2.1).
fn substr(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let size = agent.heap.string_len(s);
    let start = relative(ops::to_integer_or_infinity(agent, call.arg(0))?, size);
    let length = match call.arg(1) {
        Value::Undefined => size,
        v => clamp(ops::to_integer_or_infinity(agent, v)?, size),
    };
    let end = (start + length).min(size);
    if start >= end {
        return Ok(Value::String(agent.heap.empty_string()));
    }
    Ok(Value::String(agent.heap.substring(s, start, end)))
}

fn split(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::require_object_coercible(agent, call.this)?;
    let (separator, limit) = (call.arg(0), call.arg(1));
    let symbol = agent.symbols.split;
    if let Some(r) = delegate(agent, separator, symbol, &[o, limit])? {
        return Ok(r);
    }
    let string = ops::to_string(agent, o)?;
    let lim = match limit {
        Value::Undefined => u32::MAX as usize,
        v => ops::to_uint32(agent, v)? as usize,
    };
    let r = to_units(agent, separator)?;
    if lim == 0 {
        return Ok(Value::Object(ops::create_array_from_list(agent, &[])));
    }
    if separator.is_undefined() {
        let a = ops::create_array_from_list(agent, &[Value::String(string)]);
        return Ok(Value::Object(a));
    }
    let units = units_of(agent, string);
    if r.is_empty() {
        let out_len = lim.min(units.len());
        let parts: Vec<Value> = units[..out_len]
            .iter()
            .map(|&u| Value::String(agent.heap.alloc_string(&[u])))
            .collect();
        return Ok(Value::Object(ops::create_array_from_list(agent, &parts)));
    }
    if units.is_empty() {
        let a = ops::create_array_from_list(agent, &[Value::String(string)]);
        return Ok(Value::Object(a));
    }
    let mut parts = Vec::new();
    let mut i = 0;
    let mut j = string_index_of(&units, &r, 0);
    while let Some(p) = j {
        parts.push(Value::String(agent.heap.substring(string, i, p)));
        if parts.len() == lim {
            return Ok(Value::Object(ops::create_array_from_list(agent, &parts)));
        }
        i = p + r.len();
        j = string_index_of(&units, &r, i);
    }
    parts.push(Value::String(agent.heap.substring(string, i, units.len())));
    Ok(Value::Object(ops::create_array_from_list(agent, &parts)))
}

/// The full, locale-free case mapping of a string.
fn map_case(agent: &mut Agent, call: NativeCall<'_>, upper: bool) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let units = units_of(agent, s);
    let cps = unicode::code_points(&units);
    let mapped = if upper {
        unicode::to_uppercase(&cps)
    } else {
        unicode::to_lowercase(&cps)
    };
    if mapped == cps {
        return Ok(Value::String(s));
    }
    string_value(agent, unicode::to_utf16(&mapped))
}

/// `toLowerCase()` and `toLocaleLowerCase()`.
fn to_lower_case(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    map_case(agent, call, false)
}

/// `toUpperCase()` and `toLocaleUpperCase()`.
fn to_upper_case(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    map_case(agent, call, true)
}

/// TrimString(string, where).
fn trim_string(agent: &mut Agent, this: Value, start: bool, end: bool) -> JsResult<Value> {
    let s = this_string(agent, this)?;
    let units = units_of(agent, s);
    let mut from = 0;
    let mut to = units.len();
    if start {
        while from < to && unicode::is_trim_space(units[from]) {
            from += 1;
        }
    }
    if end {
        while to > from && unicode::is_trim_space(units[to - 1]) {
            to -= 1;
        }
    }
    Ok(Value::String(agent.heap.substring(s, from, to)))
}

fn trim(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    trim_string(agent, call.this, true, true)
}

fn trim_start(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    trim_string(agent, call.this, true, false)
}

fn trim_end(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    trim_string(agent, call.this, false, true)
}

// ----- string iterators -----

/// The state of a string iterator: the string (`None` once exhausted) and
/// the next code unit index.
pub struct StringIteratorData {
    pub string: Option<JsString>,
    pub position: usize,
}

impl Trace for StringIteratorData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(s) = self.string {
            tracer.string(s);
        }
    }
}

/// `String.prototype[Symbol.iterator]()`.
fn string_iterator(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_string(agent, call.this)?;
    let proto = agent.intrinsic(Intrinsic::StringIteratorPrototype);
    let it = agent.heap.create_object(
        Some(proto),
        ObjectKind::StringIterator(Box::new(StringIteratorData {
            string: Some(s),
            position: 0,
        })),
    );
    Ok(Value::Object(it))
}

/// `%StringIteratorPrototype%.next()`: the next code point as a string.
fn string_iterator_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(it) = call.this else {
        return agent.type_error("not a string iterator");
    };
    let (string, position) = match &agent.heap.object(it).kind {
        ObjectKind::StringIterator(d) => (d.string, d.position),
        _ => return agent.type_error("not a string iterator"),
    };
    let Some(s) = string else {
        let r = ops::create_iter_result_object(agent, Value::Undefined, true);
        return Ok(Value::Object(r));
    };
    let len = agent.heap.string_len(s);
    if position >= len {
        if let ObjectKind::StringIterator(d) = &mut agent.heap.object_mut(it).kind {
            d.string = None;
        }
        let r = ops::create_iter_result_object(agent, Value::Undefined, true);
        return Ok(Value::Object(r));
    }
    let (_, size) = unicode::code_point_at(agent.heap.string_units(s), position);
    if let ObjectKind::StringIterator(d) = &mut agent.heap.object_mut(it).kind {
        d.position = position + size;
    }
    let value = Value::String(agent.heap.substring(s, position, position + size));
    Ok(Value::Object(ops::create_iter_result_object(
        agent, value, false,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::string::utf16;

    #[test]
    fn string_index_of_edges() {
        let s = utf16("abcabc");
        assert_eq!(string_index_of(&s, &utf16("c"), 0), Some(2));
        assert_eq!(string_index_of(&s, &utf16("c"), 3), Some(5));
        assert_eq!(string_index_of(&s, &utf16(""), 6), Some(6));
        assert_eq!(string_index_of(&s, &utf16(""), 7), None);
        assert_eq!(string_index_of(&s, &utf16("abcabcd"), 0), None);
    }

    #[test]
    fn substitution_patterns() {
        let mut agent = Agent::new();
        let string = utf16("xaby");
        let sub = |agent: &mut Agent, t: &str, captures: &[Value]| {
            let r = get_substitution(
                agent,
                &utf16("ab"),
                &string,
                1,
                captures,
                Value::Undefined,
                &utf16(t),
            )
            .unwrap();
            String::from_utf16(&r).unwrap()
        };
        assert_eq!(sub(&mut agent, "[$&]", &[]), "[ab]");
        assert_eq!(sub(&mut agent, "$`|$'", &[]), "x|y");
        assert_eq!(sub(&mut agent, "$$$", &[]), "$$");
        assert_eq!(sub(&mut agent, "$1$<n>", &[]), "$1$<n>");
        let c = Value::String(agent.heap.alloc_str("C"));
        assert_eq!(sub(&mut agent, "$1-$01-$10", &[c]), "C-C-C0");
        assert_eq!(sub(&mut agent, "$2", &[c]), "$2");
    }

    #[test]
    fn clamps() {
        assert_eq!(clamp(-3.0, 5), 0);
        assert_eq!(clamp(f64::INFINITY, 5), 5);
        assert_eq!(relative(-2.0, 5), 3);
        assert_eq!(relative(f64::NEG_INFINITY, 5), 0);
    }
}
