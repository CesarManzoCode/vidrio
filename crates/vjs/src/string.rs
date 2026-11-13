//! Immutable UTF-16 strings with ropes for concatenation, and the interned
//! atoms used as property keys.
//!
//! A string is flat (its own code units), a rope (the concatenation of two
//! strings, flattened in place on first read of its code units) or a view
//! of an atom. Atoms are interned in the heap's weak atom table.

use std::mem::size_of;
use std::rc::Rc;

use crate::gc::{Atom, Heap, JsString, Trace, Tracer};
use crate::value::{MAX_ARRAY_INDEX, PropertyKey, Value};

/// Longest string the engine creates, in code units; longer results throw
/// a RangeError.
pub const MAX_STRING_LENGTH: usize = (1 << 30) - 25;

/// Concatenations shorter than this are copied instead of building a rope.
const ROPE_MIN_LENGTH: usize = 32;

pub enum StringData {
    Flat(Box<[u16]>),
    Rope {
        left: JsString,
        right: JsString,
        len: usize,
    },
    Atom(Atom),
}

impl StringData {
    pub(crate) fn extra_bytes(&self) -> usize {
        match self {
            StringData::Flat(units) => units.len() * 2,
            _ => 0,
        }
    }
}

impl Trace for StringData {
    fn trace(&self, tracer: &mut Tracer) {
        match self {
            StringData::Flat(_) => {}
            StringData::Rope { left, right, .. } => {
                tracer.string(*left);
                tracer.string(*right);
            }
            StringData::Atom(a) => {
                tracer.atom(*a);
            }
        }
    }
}

/// The code units of an interned string.
pub struct AtomData(pub(crate) Rc<[u16]>);

impl AtomData {
    pub fn units(&self) -> &[u16] {
        &self.0
    }
}

/// The array index named by `units`, if they are its canonical decimal form.
pub fn array_index(units: &[u16]) -> Option<u32> {
    if units.is_empty() || units.len() > 10 {
        return None;
    }
    if units.len() > 1 && units[0] == u16::from(b'0') {
        return None;
    }
    let mut n: u64 = 0;
    for &u in units {
        if !(u16::from(b'0')..=u16::from(b'9')).contains(&u) {
            return None;
        }
        n = n * 10 + u64::from(u - u16::from(b'0'));
    }
    if n <= u64::from(MAX_ARRAY_INDEX) {
        Some(n as u32)
    } else {
        None
    }
}

/// UTF-16 code units of a Rust string.
pub fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

impl Heap {
    /// A new flat string with a copy of `units`.
    pub fn alloc_string(&mut self, units: &[u16]) -> JsString {
        if units.is_empty() {
            return self.empty_string;
        }
        self.note_alloc(size_of::<StringData>() + units.len() * 2);
        self.strings.alloc(StringData::Flat(units.into()))
    }

    pub fn alloc_string_owned(&mut self, units: Vec<u16>) -> JsString {
        if units.is_empty() {
            return self.empty_string;
        }
        self.note_alloc(size_of::<StringData>() + units.len() * 2);
        self.strings
            .alloc(StringData::Flat(units.into_boxed_slice()))
    }

    pub fn alloc_str(&mut self, s: &str) -> JsString {
        self.alloc_string_owned(utf16(s))
    }

    pub fn empty_string(&self) -> JsString {
        self.empty_string
    }

    /// A string whose code units are those of `a`.
    pub fn atom_string(&mut self, a: Atom) -> JsString {
        if self.atoms.get(a).0.is_empty() {
            return self.empty_string;
        }
        self.note_alloc(size_of::<StringData>());
        self.strings.alloc(StringData::Atom(a))
    }

    pub fn string_len(&self, s: JsString) -> usize {
        match self.strings.get(s) {
            StringData::Flat(units) => units.len(),
            StringData::Rope { len, .. } => *len,
            StringData::Atom(a) => self.atoms.get(*a).0.len(),
        }
    }

    /// The concatenation of `a` and `b`, or `None` if it would be longer
    /// than [`MAX_STRING_LENGTH`].
    pub fn concat(&mut self, a: JsString, b: JsString) -> Option<JsString> {
        let (la, lb) = (self.string_len(a), self.string_len(b));
        if la == 0 {
            return Some(b);
        }
        if lb == 0 {
            return Some(a);
        }
        let len = la + lb;
        if len > MAX_STRING_LENGTH {
            return None;
        }
        if len < ROPE_MIN_LENGTH {
            let mut units = Vec::with_capacity(len);
            self.append_units(a, &mut units);
            self.append_units(b, &mut units);
            return Some(self.alloc_string_owned(units));
        }
        self.note_alloc(size_of::<StringData>());
        Some(self.strings.alloc(StringData::Rope {
            left: a,
            right: b,
            len,
        }))
    }

    /// Appends the code units of `s` to `out`, walking ropes without
    /// recursion.
    fn append_units(&self, s: JsString, out: &mut Vec<u16>) {
        let mut stack = vec![s];
        while let Some(x) = stack.pop() {
            match self.strings.get(x) {
                StringData::Flat(units) => out.extend_from_slice(units),
                StringData::Atom(a) => out.extend_from_slice(&self.atoms.get(*a).0),
                StringData::Rope { left, right, .. } => {
                    stack.push(*right);
                    stack.push(*left);
                }
            }
        }
    }

    /// Replaces a rope by its flat contents (the handle stays the same).
    pub fn flatten(&mut self, s: JsString) {
        if let StringData::Rope { len, .. } = self.strings.get(s) {
            let mut units = Vec::with_capacity(*len);
            self.append_units(s, &mut units);
            *self.strings.get_mut(s) = StringData::Flat(units.into_boxed_slice());
        }
    }

    /// Code units of a string that is not a rope (call [`Heap::flatten`]
    /// first). Panics on a rope.
    pub fn flat(&self, s: JsString) -> &[u16] {
        match self.strings.get(s) {
            StringData::Flat(units) => units,
            StringData::Atom(a) => &self.atoms.get(*a).0,
            StringData::Rope { .. } => panic!("string {s:?} is a rope; flatten it first"),
        }
    }

    pub fn string_units(&mut self, s: JsString) -> &[u16] {
        self.flatten(s);
        self.flat(s)
    }

    /// The string as Rust text (lone surrogates become U+FFFD).
    pub fn string_to_rust(&mut self, s: JsString) -> String {
        String::from_utf16_lossy(self.string_units(s))
    }

    /// Whether two strings have the same code units.
    pub fn string_eq(&mut self, a: JsString, b: JsString) -> bool {
        if a == b {
            return true;
        }
        if self.string_len(a) != self.string_len(b) {
            return false;
        }
        self.flatten(a);
        self.flatten(b);
        self.flat(a) == self.flat(b)
    }

    /// Code-unit order of two strings.
    pub fn string_cmp(&mut self, a: JsString, b: JsString) -> std::cmp::Ordering {
        self.flatten(a);
        self.flatten(b);
        self.flat(a).cmp(self.flat(b))
    }

    pub fn code_unit_at(&mut self, s: JsString, index: usize) -> Option<u16> {
        self.string_units(s).get(index).copied()
    }

    /// The code units `start..end` of `s` as a new string.
    pub fn substring(&mut self, s: JsString, start: usize, end: usize) -> JsString {
        if start == 0 && end == self.string_len(s) {
            return s;
        }
        self.flatten(s);
        let units: Vec<u16> = self.flat(s)[start..end].to_vec();
        self.alloc_string_owned(units)
    }

    /// The atom with these code units, interning it if needed.
    pub fn intern(&mut self, units: &[u16]) -> Atom {
        if let Some(a) = self.atom_table.get(units) {
            return *a;
        }
        let rc: Rc<[u16]> = Rc::from(units);
        self.note_alloc(size_of::<AtomData>() + units.len() * 2);
        let atom = self.atoms.alloc(AtomData(rc.clone()));
        self.atom_table.insert(rc, atom);
        atom
    }

    pub fn intern_str(&mut self, s: &str) -> Atom {
        self.intern(&utf16(s))
    }

    pub fn atom_units(&self, a: Atom) -> &[u16] {
        &self.atoms.get(a).0
    }

    pub fn atom_to_rust(&self, a: Atom) -> String {
        String::from_utf16_lossy(&self.atoms.get(a).0)
    }

    /// The atom with the code units of `s`.
    pub fn string_to_atom(&mut self, s: JsString) -> Atom {
        if let StringData::Atom(a) = self.strings.get(s) {
            return *a;
        }
        self.flatten(s);
        if let Some(a) = self.atom_table.get(self.flat(s)) {
            return *a;
        }
        let units: Vec<u16> = self.flat(s).to_vec();
        self.intern(&units)
    }

    /// The property key named by these code units.
    pub fn key_from_units(&mut self, units: &[u16]) -> PropertyKey {
        match array_index(units) {
            Some(i) => PropertyKey::Index(i),
            None => PropertyKey::String(self.intern(units)),
        }
    }

    pub fn key_from_str(&mut self, s: &str) -> PropertyKey {
        self.key_from_units(&utf16(s))
    }

    /// The property key named by the string `s`.
    pub fn key_from_string(&mut self, s: JsString) -> PropertyKey {
        if let StringData::Atom(a) = self.strings.get(s) {
            return PropertyKey::String(*a);
        }
        self.flatten(s);
        if let Some(i) = array_index(self.flat(s)) {
            return PropertyKey::Index(i);
        }
        PropertyKey::String(self.string_to_atom(s))
    }

    /// The key as a language value (a string or a symbol).
    pub fn key_to_value(&mut self, key: PropertyKey) -> Value {
        match key {
            PropertyKey::Index(i) => Value::String(self.alloc_str(&i.to_string())),
            PropertyKey::String(a) => Value::String(self.atom_string(a)),
            PropertyKey::Symbol(s) => Value::Symbol(s),
        }
    }

    /// Code units of a string key (indices in decimal); `None` for symbols.
    pub fn key_units(&self, key: PropertyKey) -> Option<Vec<u16>> {
        match key {
            PropertyKey::Index(i) => Some(utf16(&i.to_string())),
            PropertyKey::String(a) => Some(self.atoms.get(a).0.to_vec()),
            PropertyKey::Symbol(_) => None,
        }
    }

    /// The key as Rust text, for messages.
    pub fn key_to_rust(&mut self, key: PropertyKey) -> String {
        match key {
            PropertyKey::Index(i) => i.to_string(),
            PropertyKey::String(a) => self.atom_to_rust(a),
            PropertyKey::Symbol(s) => {
                let description = self.symbol(s).description;
                match description {
                    Some(d) => format!("Symbol({})", self.string_to_rust(d)),
                    None => "Symbol()".to_string(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_index_is_canonical() {
        assert_eq!(array_index(&utf16("0")), Some(0));
        assert_eq!(array_index(&utf16("4294967294")), Some(4294967294));
        assert_eq!(array_index(&utf16("4294967295")), None);
        assert_eq!(array_index(&utf16("01")), None);
        assert_eq!(array_index(&utf16("")), None);
        assert_eq!(array_index(&utf16("-1")), None);
        assert_eq!(array_index(&utf16("1.5")), None);
    }

    #[test]
    fn ropes_flatten_to_the_concatenation() {
        let mut heap = Heap::new();
        let a = heap.alloc_str(&"a".repeat(40));
        let b = heap.alloc_str(&"b".repeat(40));
        let ab = heap.concat(a, b).unwrap();
        let abab = heap.concat(ab, ab).unwrap();
        assert_eq!(heap.string_len(abab), 160);
        let expected = format!("{}{}", "a".repeat(40), "b".repeat(40)).repeat(2);
        assert_eq!(heap.string_to_rust(abab), expected);
    }

    #[test]
    fn interning_returns_the_same_atom() {
        let mut heap = Heap::new();
        let x = heap.intern_str("hello");
        let s = heap.alloc_str("hello");
        assert_eq!(heap.string_to_atom(s), x);
        assert_eq!(heap.key_from_str("7"), PropertyKey::Index(7));
        assert_eq!(heap.key_from_str("hello"), PropertyKey::String(x));
    }
}
