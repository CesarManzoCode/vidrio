//! `RegExp` (ECMA-262 §22.2.4–§22.2.9): the constructor, `RegExp.escape`,
//! `RegExp[@@species]`, the prototype with `exec`, `test`, `toString`, the
//! flag getters, `@@match`, `@@matchAll`, `@@replace`, `@@search` and
//! `@@split`, `%RegExpStringIteratorPrototype%`, and `compile` of Annex B.
//! The legacy static properties (`RegExp.$1`, …) belong to the
//! `legacy-regexp` proposal and are not provided.
//!
//! The engine itself is [`crate::regexp`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::agent::Agent;
use crate::builtins::string::get_substitution;
use crate::builtins::{
    accessor, constructor, intrinsic, method, plain_object, set_intrinsic, str_value, to_string_tag,
};
use crate::completion::JsResult;
use crate::function::{NativeCall, NativeFn, create_builtin_function};
use crate::gc::{JsString, Object, Realm, Symbol, Trace, Tracer};
use crate::internal;
use crate::object::{Attributes, ObjectKind, Slot};
use crate::ops;
use crate::realm::Intrinsic;
use crate::regexp::{Flags, Regex, advance_string_index};
use crate::string::{MAX_STRING_LENGTH, utf16};
use crate::unicode;
use crate::value::{PropertyKey, Value};

/// The internal slots of a RegExp object: `[[OriginalSource]]`,
/// `[[OriginalFlags]]` and `[[RegExpMatcher]]`.
pub struct RegExpData {
    pub source: JsString,
    pub flags: JsString,
    pub regex: Rc<Regex>,
}

impl Trace for RegExpData {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.string(self.source);
        tracer.string(self.flags);
    }
}

/// The state of a `%RegExpStringIteratorPrototype%` instance.
pub struct RegExpStringIteratorData {
    pub matcher: Object,
    pub string: JsString,
    pub global: bool,
    pub unicode: bool,
    pub done: bool,
}

impl Trace for RegExpStringIteratorData {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.object(self.matcher);
        tracer.string(self.string);
    }
}

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::RegExpPrototype, proto);
    let ctor = constructor(agent, realm, "RegExp", regexp_constructor, 2, proto, None);
    set_intrinsic(agent, realm, Intrinsic::RegExp, ctor);
    method(agent, realm, ctor, "escape", escape, 1);
    let species = PropertyKey::Symbol(agent.symbols.species);
    accessor(agent, realm, ctor, species, Some(get_species), None);

    let methods: &[(&str, NativeFn, u32)] = &[
        ("compile", compile, 2),
        ("exec", exec, 1),
        ("test", test, 1),
        ("toString", to_string, 0),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    let getters: &[(&str, NativeFn)] = &[
        ("dotAll", get_dot_all),
        ("flags", get_flags),
        ("global", get_global),
        ("hasIndices", get_has_indices),
        ("ignoreCase", get_ignore_case),
        ("multiline", get_multiline),
        ("source", get_source),
        ("sticky", get_sticky),
        ("unicode", get_unicode),
        ("unicodeSets", get_unicode_sets),
    ];
    for &(name, f) in getters {
        let key = agent.heap.key_from_str(name);
        accessor(agent, realm, proto, key, Some(f), None);
    }
    let s = agent.symbols;
    let symbol_methods: &[(Symbol, NativeFn, u32)] = &[
        (s.match_, symbol_match, 1),
        (s.match_all, symbol_match_all, 1),
        (s.replace, symbol_replace, 2),
        (s.search, symbol_search, 1),
        (s.split, symbol_split, 2),
    ];
    for &(sym, f, length) in symbol_methods {
        let key = PropertyKey::Symbol(sym);
        let func = create_builtin_function(agent, f, length, key, Some(realm), None, None);
        ops::define_builtin_method(agent, proto, key, func);
    }

    let iterator_proto = intrinsic(agent, realm, Intrinsic::IteratorPrototype);
    let rsi_proto = plain_object(agent, iterator_proto);
    method(agent, realm, rsi_proto, "next", string_iterator_next, 0);
    to_string_tag(agent, rsi_proto, "RegExp String Iterator");
    set_intrinsic(
        agent,
        realm,
        Intrinsic::RegExpStringIteratorPrototype,
        rsi_proto,
    );
}

// ----- helpers -----

fn units_of(agent: &mut Agent, s: JsString) -> Vec<u16> {
    agent.heap.string_units(s).to_vec()
}

fn last_index_key(agent: &Agent) -> PropertyKey {
    PropertyKey::String(agent.heap.names.last_index)
}

fn string_value(agent: &mut Agent, units: Vec<u16>) -> JsResult<Value> {
    if units.len() > MAX_STRING_LENGTH {
        return agent.range_error("string too long");
    }
    Ok(Value::String(agent.heap.alloc_string_owned(units)))
}

/// The slots of a RegExp object, if `v` is one.
fn regexp_slots(agent: &Agent, v: Value) -> Option<(JsString, JsString, Rc<Regex>)> {
    let Value::Object(o) = v else {
        return None;
    };
    match &agent.heap.object(o).kind {
        ObjectKind::RegExp(d) => Some((d.source, d.flags, d.regex.clone())),
        _ => None,
    }
}

/// Whether `o` has a `[[RegExpMatcher]]` slot.
pub(crate) fn is_regexp_object(agent: &Agent, o: Object) -> bool {
    matches!(agent.heap.object(o).kind, ObjectKind::RegExp(_))
}

/// `this` as an object, or a TypeError.
fn this_object(agent: &mut Agent, this: Value, what: &str) -> JsResult<Object> {
    match this {
        Value::Object(o) => Ok(o),
        _ => agent.type_error(&format!("{what} called on a non-object")),
    }
}

/// `this` as a RegExp object, or a TypeError.
fn this_regexp(agent: &mut Agent, this: Value, what: &str) -> JsResult<Object> {
    match this {
        Value::Object(o) if is_regexp_object(agent, o) => Ok(o),
        _ => agent.type_error(&format!("{what} called on an incompatible receiver")),
    }
}

thread_local! {
    /// Compiled patterns by (pattern, flags), so that a literal in a loop
    /// is parsed once.
    static CACHE: RefCell<HashMap<(Vec<u16>, Vec<u16>), Rc<Regex>>> =
        RefCell::new(HashMap::new());
}

const CACHE_LIMIT: usize = 512;

/// Parses and compiles a pattern with its flags (the checks of
/// RegExpInitialize).
fn compile_pattern(agent: &mut Agent, pattern: &[u16], flags: &[u16]) -> JsResult<Rc<Regex>> {
    let key = (pattern.to_vec(), flags.to_vec());
    if let Some(r) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return Ok(r);
    }
    let Some(f) = Flags::parse(flags) else {
        let flags = String::from_utf16_lossy(flags);
        return agent.syntax_error(&format!("invalid regular expression flags '{flags}'"));
    };
    match Regex::new(pattern, f) {
        Ok(r) => {
            let r = Rc::new(r);
            CACHE.with(|c| {
                let mut c = c.borrow_mut();
                if c.len() >= CACHE_LIMIT {
                    c.clear();
                }
                c.insert(key, r.clone());
            });
            Ok(r)
        }
        Err(message) => agent.syntax_error(&format!("invalid regular expression: {message}")),
    }
}

/// The conversions and checks of RegExpInitialize(obj, pattern, flags):
/// the new `[[OriginalSource]]`, `[[OriginalFlags]]` and matcher.
fn initialize(agent: &mut Agent, pattern: Value, flags: Value) -> JsResult<RegExpData> {
    let source = if pattern.is_undefined() {
        agent.heap.empty_string()
    } else {
        ops::to_string(agent, pattern)?
    };
    let flag_string = if flags.is_undefined() {
        agent.heap.empty_string()
    } else {
        ops::to_string(agent, flags)?
    };
    let p = units_of(agent, source);
    let f = units_of(agent, flag_string);
    let regex = compile_pattern(agent, &p, &f)?;
    Ok(RegExpData {
        source,
        flags: flag_string,
        regex,
    })
}

/// RegExpAlloc and RegExpInitialize for a fresh object: `lastIndex` is
/// `{ [[Writable]]: true, [[Enumerable]]: false, [[Configurable]]: false }`.
fn regexp_create_object(agent: &mut Agent, proto: Object, data: RegExpData) -> Object {
    let o = agent
        .heap
        .create_object(Some(proto), ObjectKind::RegExp(Box::new(data)));
    let key = last_index_key(agent);
    agent.heap.set_own(
        o,
        key,
        Attributes::data(true, false, false),
        Slot::Data(Value::Number(0.0)),
    );
    o
}

/// RegExpCreate(P, F) with `%RegExp%` of the current realm.
pub(crate) fn regexp_create(agent: &mut Agent, pattern: Value, flags: Value) -> JsResult<Object> {
    let data = initialize(agent, pattern, flags)?;
    let proto = agent.intrinsic(Intrinsic::RegExpPrototype);
    Ok(regexp_create_object(agent, proto, data))
}

/// AdvanceStringIndex on a `lastIndex` value.
fn advance(s: &[u16], index: f64, unicode: bool) -> f64 {
    if index + 1.0 >= s.len() as f64 {
        return index + 1.0;
    }
    advance_string_index(s, index as usize, unicode) as f64
}

/// `? ToLength(? Get(rx, "lastIndex"))`.
fn get_last_index(agent: &mut Agent, rx: Object) -> JsResult<f64> {
    let key = last_index_key(agent);
    let v = ops::get(agent, rx, key)?;
    ops::to_length(agent, v)
}

/// `? Set(rx, "lastIndex", value, true)`.
fn set_last_index(agent: &mut Agent, rx: Object, value: f64) -> JsResult<()> {
    let key = last_index_key(agent);
    ops::set(agent, rx, key, Value::Number(value), true)
}

/// `? ToString(? Get(o, name))` as code units.
fn get_string(agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<Vec<u16>> {
    let v = ops::get(agent, o, key)?;
    let s = ops::to_string(agent, v)?;
    Ok(units_of(agent, s))
}

fn flags_of(agent: &mut Agent, rx: Object) -> JsResult<Vec<u16>> {
    let key = agent.heap.key_from_str("flags");
    get_string(agent, rx, key)
}

fn contains_unit(units: &[u16], c: char) -> bool {
    units.contains(&(c as u16))
}

// ----- constructor and statics -----

/// `RegExp(pattern, flags)` (§22.2.4.1).
fn regexp_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let pattern = call.arg(0);
    let flags = call.arg(1);
    let pattern_is_regexp = crate::builtins::string::is_regexp(agent, pattern)?;
    let new_target = match call.new_target {
        Some(nt) => nt,
        None => {
            let nt = call.callee;
            if pattern_is_regexp
                && flags.is_undefined()
                && let Value::Object(po) = pattern
            {
                let key = PropertyKey::String(agent.heap.names.constructor);
                let pattern_constructor = ops::get(agent, po, key)?;
                if ops::same_value(agent, Value::Object(nt), pattern_constructor) {
                    return Ok(pattern);
                }
            }
            nt
        }
    };
    let (p, f) = if let Some((source, original_flags, _)) = regexp_slots(agent, pattern) {
        let f = if flags.is_undefined() {
            Value::String(original_flags)
        } else {
            flags
        };
        (Value::String(source), f)
    } else if pattern_is_regexp && let Value::Object(po) = pattern {
        let key = agent.heap.key_from_str("source");
        let source = ops::get(agent, po, key)?;
        let f = if flags.is_undefined() {
            let key = agent.heap.key_from_str("flags");
            ops::get(agent, po, key)?
        } else {
            flags
        };
        (source, f)
    } else {
        (pattern, flags)
    };
    let proto = ops::get_prototype_from_constructor(agent, new_target, Intrinsic::RegExpPrototype)?;
    let data = initialize(agent, p, f)?;
    Ok(Value::Object(regexp_create_object(agent, proto, data)))
}

/// `get RegExp[@@species]`.
fn get_species(_agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(call.this)
}

/// EncodeForRegExpEscape(c).
fn encode_for_escape(out: &mut Vec<u16>, c: u32) {
    let push_str = |out: &mut Vec<u16>, s: &str| out.extend(s.encode_utf16());
    if let Some(ch) = char::from_u32(c)
        && ch.is_ascii()
        && "^$\\.*+?()[]{}|/".contains(ch)
    {
        out.push(u16::from(b'\\'));
        out.push(c as u16);
        return;
    }
    let control = match c {
        0x09 => Some('t'),
        0x0A => Some('n'),
        0x0B => Some('v'),
        0x0C => Some('f'),
        0x0D => Some('r'),
        _ => None,
    };
    if let Some(letter) = control {
        out.push(u16::from(b'\\'));
        out.push(letter as u16);
        return;
    }
    let punctuator = matches!(char::from_u32(c), Some(ch) if ",-=<>#&!%:;@~'`\"".contains(ch));
    let space = c <= 0xFFFF && (unicode::is_trim_space(c as u16) || (0xD800..=0xDFFF).contains(&c));
    if punctuator || space {
        if c <= 0xFF {
            push_str(out, &format!("\\x{c:02x}"));
            return;
        }
        let mut units = Vec::new();
        unicode::push_utf16(&mut units, c);
        for u in units {
            push_str(out, &format!("\\u{u:04x}"));
        }
        return;
    }
    unicode::push_utf16(out, c);
}

/// `RegExp.escape(S)`.
fn escape(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::String(s) = call.arg(0) else {
        return agent.type_error("RegExp.escape requires a string");
    };
    let units = units_of(agent, s);
    let mut out = Vec::with_capacity(units.len());
    for c in unicode::code_points(&units) {
        let alnum = c < 128 && (c as u8).is_ascii_alphanumeric();
        if out.is_empty() && alnum {
            out.extend(format!("\\x{c:02x}").encode_utf16());
        } else {
            encode_for_escape(&mut out, c);
        }
    }
    string_value(agent, out)
}

// ----- RegExp.prototype -----

/// `RegExp.prototype.compile(pattern, flags)` (Annex B.2.4.1).
fn compile(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_regexp(agent, call.this, "RegExp.prototype.compile")?;
    let (pattern, flags) = (call.arg(0), call.arg(1));
    let (p, f) = if let Some((source, original_flags, _)) = regexp_slots(agent, pattern) {
        if !flags.is_undefined() {
            return agent.type_error("flags must be undefined when the pattern is a RegExp");
        }
        (Value::String(source), Value::String(original_flags))
    } else {
        (pattern, flags)
    };
    let data = initialize(agent, p, f)?;
    if let ObjectKind::RegExp(d) = &mut agent.heap.object_mut(o).kind {
        **d = data;
    }
    set_last_index(agent, o, 0.0)?;
    Ok(Value::Object(o))
}

/// RegExpBuiltinExec(R, S).
pub(crate) fn builtin_exec(agent: &mut Agent, r: Object, s: JsString) -> JsResult<Value> {
    let last_index = get_last_index(agent, r)?;
    let Some((_, _, regex)) = regexp_slots(agent, Value::Object(r)) else {
        return agent.type_error("not a RegExp object");
    };
    let flags = regex.flags;
    let global_or_sticky = flags.global || flags.sticky;
    let last_index = if global_or_sticky { last_index } else { 0.0 };
    agent.heap.flatten(s);
    let length = agent.heap.string_len(s);
    if last_index > length as f64 {
        if global_or_sticky {
            set_last_index(agent, r, 0.0)?;
        }
        return Ok(Value::Null);
    }
    let found = regex.exec(agent.heap.flat(s), last_index as usize, flags.sticky);
    let found = match found {
        Ok(m) => m,
        Err(_) => return agent.range_error("regular expression backtracking stack exhausted"),
    };
    let Some(m) = found else {
        if global_or_sticky {
            set_last_index(agent, r, 0.0)?;
        }
        return Ok(Value::Null);
    };
    let e = m.end;
    if global_or_sticky {
        set_last_index(agent, r, e as f64)?;
    }
    let n = regex.group_count;
    let a = internal::array_create(agent, (n + 1) as f64, None)?;
    let key = PropertyKey::String(agent.heap.names.index);
    ops::create_data_property_or_throw(agent, a, key, Value::Number(m.index as f64))?;
    let key = PropertyKey::String(agent.heap.names.input);
    ops::create_data_property_or_throw(agent, a, key, Value::String(s))?;
    let start = m.index.min(e);
    let matched = Value::String(agent.heap.substring(s, start, e));
    ops::create_data_property_or_throw(agent, a, PropertyKey::Index(0), matched)?;
    let has_groups = regex.has_named_groups();
    let groups = if has_groups {
        Value::Object(ops::ordinary_object_create(agent, None))
    } else {
        Value::Undefined
    };
    let key = agent.heap.key_from_str("groups");
    ops::create_data_property_or_throw(agent, a, key, groups)?;
    let mut indices: Vec<Option<(usize, usize)>> = vec![Some((start, e))];
    let mut group_names: Vec<Option<Vec<u16>>> = Vec::with_capacity(n);
    let mut matched_names: Vec<Vec<u16>> = Vec::new();
    for i in 1..=n {
        let capture = m.captures[i - 1];
        let value = match capture {
            Some((cs, ce)) => Value::String(agent.heap.substring(s, cs, ce)),
            None => Value::Undefined,
        };
        indices.push(capture);
        let key = ops::index_key(agent, i as u64);
        ops::create_data_property_or_throw(agent, a, key, value)?;
        match (&regex.group_names[i - 1], groups) {
            (Some(name), Value::Object(g)) => {
                if matched_names.contains(name) {
                    group_names.push(None);
                } else {
                    if capture.is_some() {
                        matched_names.push(name.clone());
                    }
                    let key = agent.heap.key_from_units(name);
                    ops::create_data_property_or_throw(agent, g, key, value)?;
                    group_names.push(Some(name.clone()));
                }
            }
            _ => group_names.push(None),
        }
    }
    if flags.has_indices {
        let pairs = make_indices_array(agent, &indices, &group_names, has_groups)?;
        let key = agent.heap.key_from_str("indices");
        ops::create_data_property_or_throw(agent, a, key, Value::Object(pairs))?;
    }
    Ok(Value::Object(a))
}

/// MakeMatchIndicesIndexPairArray(S, indices, groupNames, hasGroups).
fn make_indices_array(
    agent: &mut Agent,
    indices: &[Option<(usize, usize)>],
    group_names: &[Option<Vec<u16>>],
    has_groups: bool,
) -> JsResult<Object> {
    let a = internal::array_create(agent, indices.len() as f64, None)?;
    let groups = if has_groups {
        Value::Object(ops::ordinary_object_create(agent, None))
    } else {
        Value::Undefined
    };
    let key = agent.heap.key_from_str("groups");
    ops::create_data_property_or_throw(agent, a, key, groups)?;
    for (i, pair) in indices.iter().enumerate() {
        let value = match pair {
            Some((s, e)) => Value::Object(ops::create_array_from_list(
                agent,
                &[Value::Number(*s as f64), Value::Number(*e as f64)],
            )),
            None => Value::Undefined,
        };
        let key = ops::index_key(agent, i as u64);
        ops::create_data_property_or_throw(agent, a, key, value)?;
        if i > 0
            && let (Some(name), Value::Object(g)) = (&group_names[i - 1], groups)
        {
            let key = agent.heap.key_from_units(name);
            ops::create_data_property_or_throw(agent, g, key, value)?;
        }
    }
    Ok(a)
}

/// RegExpExec(R, S).
pub(crate) fn regexp_exec(agent: &mut Agent, r: Object, s: JsString) -> JsResult<Value> {
    let key = agent.heap.key_from_str("exec");
    let exec = ops::get(agent, r, key)?;
    if ops::is_callable(agent, exec) {
        let result = ops::call(agent, exec, Value::Object(r), &[Value::String(s)])?;
        if !matches!(result, Value::Object(_) | Value::Null) {
            return agent.type_error("exec must return an object or null");
        }
        return Ok(result);
    }
    if !is_regexp_object(agent, r) {
        return agent.type_error("RegExpExec called on an incompatible receiver");
    }
    builtin_exec(agent, r, s)
}

/// `RegExp.prototype.exec(string)`.
fn exec(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = this_regexp(agent, call.this, "RegExp.prototype.exec")?;
    let s = ops::to_string(agent, call.arg(0))?;
    builtin_exec(agent, r, s)
}

/// `RegExp.prototype.test(S)`.
fn test(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = this_object(agent, call.this, "RegExp.prototype.test")?;
    let s = ops::to_string(agent, call.arg(0))?;
    let m = regexp_exec(agent, r, s)?;
    Ok(Value::Boolean(!m.is_null()))
}

/// `RegExp.prototype.toString()`.
fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = this_object(agent, call.this, "RegExp.prototype.toString")?;
    let key = agent.heap.key_from_str("source");
    let pattern = get_string(agent, r, key)?;
    let flags = flags_of(agent, r)?;
    let mut out = Vec::with_capacity(pattern.len() + flags.len() + 2);
    out.push(u16::from(b'/'));
    out.extend_from_slice(&pattern);
    out.push(u16::from(b'/'));
    out.extend_from_slice(&flags);
    string_value(agent, out)
}

/// `get RegExp.prototype.flags`.
fn get_flags(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = this_object(agent, call.this, "RegExp.prototype.flags getter")?;
    let mut out = String::new();
    for (c, name) in [
        ('d', "hasIndices"),
        ('g', "global"),
        ('i', "ignoreCase"),
        ('m', "multiline"),
        ('s', "dotAll"),
        ('u', "unicode"),
        ('v', "unicodeSets"),
        ('y', "sticky"),
    ] {
        let key = agent.heap.key_from_str(name);
        let v = ops::get(agent, r, key)?;
        if ops::to_boolean(agent, v) {
            out.push(c);
        }
    }
    Ok(str_value(agent, &out))
}

/// RegExpHasFlag(R, codeUnit).
fn has_flag(agent: &mut Agent, this: Value, pick: fn(Flags) -> bool) -> JsResult<Value> {
    let Value::Object(r) = this else {
        return agent.type_error("RegExp flag getter called on a non-object");
    };
    if let Some((_, _, regex)) = regexp_slots(agent, this) {
        return Ok(Value::Boolean(pick(regex.flags)));
    }
    if r == agent.intrinsic(Intrinsic::RegExpPrototype) {
        return Ok(Value::Undefined);
    }
    agent.type_error("RegExp flag getter called on an incompatible receiver")
}

fn get_dot_all(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    has_flag(agent, call.this, |f| f.dot_all)
}

fn get_global(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    has_flag(agent, call.this, |f| f.global)
}

fn get_has_indices(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    has_flag(agent, call.this, |f| f.has_indices)
}

fn get_ignore_case(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    has_flag(agent, call.this, |f| f.ignore_case)
}

fn get_multiline(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    has_flag(agent, call.this, |f| f.multiline)
}

fn get_sticky(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    has_flag(agent, call.this, |f| f.sticky)
}

fn get_unicode(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    has_flag(agent, call.this, |f| f.unicode)
}

fn get_unicode_sets(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    has_flag(agent, call.this, |f| f.unicode_sets)
}

/// EscapeRegExpPattern(P, F): a source that reads back as the same
/// pattern inside a RegularExpressionLiteral.
pub fn escape_pattern(source: &[u16]) -> Vec<u16> {
    if source.is_empty() {
        return utf16("(?:)");
    }
    let mut out = Vec::with_capacity(source.len());
    let mut escaped = false;
    for &u in source {
        let terminator = match u {
            0x0A => Some("n"),
            0x0D => Some("r"),
            0x2028 => Some("u2028"),
            0x2029 => Some("u2029"),
            _ => None,
        };
        if let Some(t) = terminator {
            if !escaped {
                out.push(u16::from(b'\\'));
            }
            out.extend(t.encode_utf16());
            escaped = false;
            continue;
        }
        if escaped {
            out.push(u);
            escaped = false;
        } else if u == u16::from(b'\\') {
            out.push(u);
            escaped = true;
        } else if u == u16::from(b'/') {
            out.push(u16::from(b'\\'));
            out.push(u);
        } else {
            out.push(u);
        }
    }
    out
}

/// `get RegExp.prototype.source`.
fn get_source(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(r) = call.this else {
        return agent.type_error("RegExp.prototype.source getter called on a non-object");
    };
    let Some((source, _, _)) = regexp_slots(agent, call.this) else {
        if r == agent.intrinsic(Intrinsic::RegExpPrototype) {
            return Ok(str_value(agent, "(?:)"));
        }
        return agent
            .type_error("RegExp.prototype.source getter called on an incompatible receiver");
    };
    let units = units_of(agent, source);
    string_value(agent, escape_pattern(&units))
}

/// `RegExp.prototype[@@match](string)`.
fn symbol_match(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let rx = this_object(agent, call.this, "RegExp.prototype[Symbol.match]")?;
    let s = ops::to_string(agent, call.arg(0))?;
    let flags = flags_of(agent, rx)?;
    if !contains_unit(&flags, 'g') {
        return regexp_exec(agent, rx, s);
    }
    let full_unicode = contains_unit(&flags, 'u') || contains_unit(&flags, 'v');
    set_last_index(agent, rx, 0.0)?;
    let a = internal::array_create(agent, 0.0, None)?;
    let mut n = 0u64;
    loop {
        let result = regexp_exec(agent, rx, s)?;
        let Value::Object(result) = result else {
            return Ok(if n == 0 {
                Value::Null
            } else {
                Value::Object(a)
            });
        };
        let match_str = ops::get(agent, result, PropertyKey::Index(0))?;
        let match_str = ops::to_string(agent, match_str)?;
        let key = ops::index_key(agent, n);
        ops::create_data_property_or_throw(agent, a, key, Value::String(match_str))?;
        if agent.heap.string_len(match_str) == 0 {
            let this_index = get_last_index(agent, rx)?;
            let next = advance(agent.heap.string_units(s), this_index, full_unicode);
            set_last_index(agent, rx, next)?;
        }
        n += 1;
    }
}

/// `RegExp.prototype[@@matchAll](string)`.
fn symbol_match_all(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = this_object(agent, call.this, "RegExp.prototype[Symbol.matchAll]")?;
    let s = ops::to_string(agent, call.arg(0))?;
    let default = agent.intrinsic(Intrinsic::RegExp);
    let c = ops::species_constructor(agent, r, default)?;
    let flags = flags_of(agent, r)?;
    let flags_value = string_value(agent, flags.clone())?;
    let matcher = ops::construct(agent, c, &[Value::Object(r), flags_value], None)?;
    let last_index = get_last_index(agent, r)?;
    set_last_index(agent, matcher, last_index)?;
    let global = contains_unit(&flags, 'g');
    let unicode = contains_unit(&flags, 'u') || contains_unit(&flags, 'v');
    let proto = agent.intrinsic(Intrinsic::RegExpStringIteratorPrototype);
    let it = agent.heap.create_object(
        Some(proto),
        ObjectKind::RegExpStringIterator(Box::new(RegExpStringIteratorData {
            matcher,
            string: s,
            global,
            unicode,
            done: false,
        })),
    );
    Ok(Value::Object(it))
}

/// The step of `%RegExpStringIteratorPrototype%.next`: the next match, or
/// `None` when the iterator is done.
fn regexp_string_iterator_step(
    agent: &mut Agent,
    matcher: Object,
    s: JsString,
    global: bool,
    unicode: bool,
) -> JsResult<(Option<Value>, bool)> {
    let result = regexp_exec(agent, matcher, s)?;
    let Value::Object(m) = result else {
        return Ok((None, true));
    };
    if !global {
        return Ok((Some(result), true));
    }
    let match_str = ops::get(agent, m, PropertyKey::Index(0))?;
    let match_str = ops::to_string(agent, match_str)?;
    if agent.heap.string_len(match_str) == 0 {
        let this_index = get_last_index(agent, matcher)?;
        let next = advance(agent.heap.string_units(s), this_index, unicode);
        set_last_index(agent, matcher, next)?;
    }
    Ok((Some(result), false))
}

/// `%RegExpStringIteratorPrototype%.next()`.
fn string_iterator_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(it) = call.this else {
        return agent.type_error("not a RegExp String Iterator");
    };
    let state = match &agent.heap.object(it).kind {
        ObjectKind::RegExpStringIterator(d) => (d.matcher, d.string, d.global, d.unicode, d.done),
        _ => return agent.type_error("not a RegExp String Iterator"),
    };
    let (matcher, s, global, unicode, done) = state;
    if done {
        let r = ops::create_iter_result_object(agent, Value::Undefined, true);
        return Ok(Value::Object(r));
    }
    let step = regexp_string_iterator_step(agent, matcher, s, global, unicode);
    let finished = !matches!(step, Ok((_, false)));
    if finished && let ObjectKind::RegExpStringIterator(d) = &mut agent.heap.object_mut(it).kind {
        d.done = true;
    }
    let (value, _) = step?;
    let r = match value {
        Some(v) => ops::create_iter_result_object(agent, v, false),
        None => ops::create_iter_result_object(agent, Value::Undefined, true),
    };
    Ok(Value::Object(r))
}

/// `RegExp.prototype[@@replace](string, replaceValue)`.
fn symbol_replace(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let rx = this_object(agent, call.this, "RegExp.prototype[Symbol.replace]")?;
    let s = ops::to_string(agent, call.arg(0))?;
    let string = units_of(agent, s);
    let length_s = string.len();
    let mut replace_value = call.arg(1);
    let functional = ops::is_callable(agent, replace_value);
    if !functional {
        replace_value = Value::String(ops::to_string(agent, replace_value)?);
    }
    let flags = flags_of(agent, rx)?;
    let global = contains_unit(&flags, 'g');
    let full_unicode = contains_unit(&flags, 'u') || contains_unit(&flags, 'v');
    if global {
        set_last_index(agent, rx, 0.0)?;
    }
    let mut results = Vec::new();
    loop {
        let result = regexp_exec(agent, rx, s)?;
        let Value::Object(result) = result else {
            break;
        };
        results.push(result);
        if !global {
            break;
        }
        let match_str = ops::get(agent, result, PropertyKey::Index(0))?;
        let match_str = ops::to_string(agent, match_str)?;
        if agent.heap.string_len(match_str) == 0 {
            let this_index = get_last_index(agent, rx)?;
            set_last_index(agent, rx, advance(&string, this_index, full_unicode))?;
        }
    }
    let template = match replace_value {
        Value::String(t) if !functional => units_of(agent, t),
        _ => Vec::new(),
    };
    let mut accumulated: Vec<u16> = Vec::new();
    let mut next_source_position = 0usize;
    for result in results {
        let result_length = ops::length_of_array_like(agent, result)?;
        let n_captures = (result_length - 1.0).max(0.0) as u64;
        let matched = ops::get(agent, result, PropertyKey::Index(0))?;
        let matched = ops::to_string(agent, matched)?;
        let matched_units = units_of(agent, matched);
        let match_length = matched_units.len();
        let key = PropertyKey::String(agent.heap.names.index);
        let position = ops::get(agent, result, key)?;
        let position = ops::to_integer_or_infinity(agent, position)?;
        let position = position.max(0.0).min(length_s as f64) as usize;
        let mut captures = Vec::new();
        for n in 1..=n_captures {
            let key = ops::index_key(agent, n);
            let cap = ops::get(agent, result, key)?;
            let cap = if cap.is_undefined() {
                cap
            } else {
                Value::String(ops::to_string(agent, cap)?)
            };
            captures.push(cap);
        }
        let key = agent.heap.key_from_str("groups");
        let named_captures = ops::get(agent, result, key)?;
        let replacement = if functional {
            let mut args = Vec::with_capacity(captures.len() + 4);
            args.push(Value::String(matched));
            args.extend_from_slice(&captures);
            args.push(Value::Number(position as f64));
            args.push(Value::String(s));
            if !named_captures.is_undefined() {
                args.push(named_captures);
            }
            let v = ops::call(agent, replace_value, Value::Undefined, &args)?;
            let v = ops::to_string(agent, v)?;
            units_of(agent, v)
        } else {
            let named = if named_captures.is_undefined() {
                named_captures
            } else {
                Value::Object(ops::to_object(agent, named_captures)?)
            };
            get_substitution(
                agent,
                &matched_units,
                &string,
                position,
                &captures,
                named,
                &template,
            )?
        };
        if position >= next_source_position {
            accumulated.extend_from_slice(&string[next_source_position..position]);
            accumulated.extend_from_slice(&replacement);
            next_source_position = position + match_length;
            if accumulated.len() > MAX_STRING_LENGTH {
                return agent.range_error("string too long");
            }
        }
    }
    if next_source_position < length_s {
        accumulated.extend_from_slice(&string[next_source_position..]);
    }
    string_value(agent, accumulated)
}

/// `RegExp.prototype[@@search](string)`.
fn symbol_search(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let rx = this_object(agent, call.this, "RegExp.prototype[Symbol.search]")?;
    let s = ops::to_string(agent, call.arg(0))?;
    let key = last_index_key(agent);
    let previous = ops::get(agent, rx, key)?;
    if !ops::same_value(agent, previous, Value::Number(0.0)) {
        ops::set(agent, rx, key, Value::Number(0.0), true)?;
    }
    let result = regexp_exec(agent, rx, s)?;
    let current = ops::get(agent, rx, key)?;
    if !ops::same_value(agent, current, previous) {
        ops::set(agent, rx, key, previous, true)?;
    }
    match result {
        Value::Object(r) => {
            let key = PropertyKey::String(agent.heap.names.index);
            ops::get(agent, r, key)
        }
        _ => Ok(Value::Number(-1.0)),
    }
}

/// `RegExp.prototype[@@split](string, limit)`.
fn symbol_split(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let rx = this_object(agent, call.this, "RegExp.prototype[Symbol.split]")?;
    let s = ops::to_string(agent, call.arg(0))?;
    let default = agent.intrinsic(Intrinsic::RegExp);
    let c = ops::species_constructor(agent, rx, default)?;
    let mut flags = flags_of(agent, rx)?;
    let unicode_matching = contains_unit(&flags, 'u') || contains_unit(&flags, 'v');
    if !contains_unit(&flags, 'y') {
        flags.push(u16::from(b'y'));
    }
    let new_flags = string_value(agent, flags)?;
    let splitter = ops::construct(agent, c, &[Value::Object(rx), new_flags], None)?;
    let a = internal::array_create(agent, 0.0, None)?;
    let mut length_a = 0u64;
    let limit = call.arg(1);
    let lim = if limit.is_undefined() {
        u64::from(u32::MAX)
    } else {
        u64::from(ops::to_uint32(agent, limit)?)
    };
    if lim == 0 {
        return Ok(Value::Object(a));
    }
    let string = units_of(agent, s);
    let size = string.len();
    if size == 0 {
        let z = regexp_exec(agent, splitter, s)?;
        if !z.is_null() {
            return Ok(Value::Object(a));
        }
        ops::create_data_property_or_throw(agent, a, PropertyKey::Index(0), Value::String(s))?;
        return Ok(Value::Object(a));
    }
    let mut p = 0usize;
    let mut q = p;
    while q < size {
        set_last_index(agent, splitter, q as f64)?;
        let z = regexp_exec(agent, splitter, s)?;
        let Value::Object(z) = z else {
            q = advance_string_index(&string, q, unicode_matching);
            continue;
        };
        let e = get_last_index(agent, splitter)?;
        let e = e.min(size as f64) as usize;
        if e == p {
            q = advance_string_index(&string, q, unicode_matching);
            continue;
        }
        let t = Value::String(agent.heap.substring(s, p, q.max(p)));
        let key = ops::index_key(agent, length_a);
        ops::create_data_property_or_throw(agent, a, key, t)?;
        length_a += 1;
        if length_a == lim {
            return Ok(Value::Object(a));
        }
        p = e;
        let number_of_captures = ops::length_of_array_like(agent, z)?;
        let number_of_captures = (number_of_captures - 1.0).max(0.0) as u64;
        for i in 1..=number_of_captures {
            let key = ops::index_key(agent, i);
            let next_capture = ops::get(agent, z, key)?;
            let key = ops::index_key(agent, length_a);
            ops::create_data_property_or_throw(agent, a, key, next_capture)?;
            length_a += 1;
            if length_a == lim {
                return Ok(Value::Object(a));
            }
        }
        q = p;
    }
    let t = Value::String(agent.heap.substring(s, p.min(size), size));
    let key = ops::index_key(agent, length_a);
    ops::create_data_property_or_throw(agent, a, key, t)?;
    Ok(Value::Object(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(s: &str) -> String {
        String::from_utf16(&escape_pattern(&utf16(s))).unwrap()
    }

    #[test]
    fn source_escaping() {
        assert_eq!(esc(""), "(?:)");
        assert_eq!(esc("a/b"), "a\\/b");
        assert_eq!(esc("\\/"), "\\/");
        assert_eq!(esc("a\nb"), "a\\nb");
        assert_eq!(esc("\\\n"), "\\n");
        assert_eq!(esc("\u{2028}"), "\\u2028");
    }

    #[test]
    fn regexp_escape_encoding() {
        let mut out = Vec::new();
        encode_for_escape(&mut out, u32::from(b'.'));
        encode_for_escape(&mut out, u32::from(b','));
        encode_for_escape(&mut out, 0x20);
        encode_for_escape(&mut out, 0x2028);
        encode_for_escape(&mut out, u32::from(b'_'));
        assert_eq!(String::from_utf16(&out).unwrap(), "\\.\\x2c\\x20\\u2028_");
    }
}
