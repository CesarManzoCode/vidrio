//! Interned names (vault/CONTRACT.md §3.3).
//!
//! An [`Atom`] is a pointer to a string that lives for the whole process.
//! Every distinct string is interned once, so two atoms are equal exactly
//! when their pointers are, and copying an atom is copying a pointer. The
//! table only grows: element, attribute and namespace names are a small,
//! bounded set in practice.

use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::{Mutex, OnceLock};

/// An interned string.
#[derive(Clone, Copy)]
pub struct Atom(&'static str);

fn table() -> &'static Mutex<HashSet<&'static str>> {
    static TABLE: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashSet::new()))
}

impl Atom {
    /// The atom for `text`, interning it the first time.
    pub fn new(text: &str) -> Atom {
        let mut set = table().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&interned) = set.get(text) {
            return Atom(interned);
        }
        let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
        set.insert(leaked);
        Atom(leaked)
    }

    /// The interned text.
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl PartialEq for Atom {
    fn eq(&self, other: &Atom) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for Atom {}

impl Hash for Atom {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0.as_ptr() as usize).hash(state);
    }
}

impl PartialEq<str> for Atom {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Atom {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl Deref for Atom {
    type Target = str;
    fn deref(&self) -> &str {
        self.0
    }
}

impl fmt::Debug for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.0, f)
    }
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl From<&str> for Atom {
    fn from(text: &str) -> Atom {
        Atom::new(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_strings_share_one_atom() {
        let a = Atom::new("div");
        let b = Atom::new(&String::from("div"));
        assert_eq!(a, b);
        assert!(std::ptr::eq(a.as_str(), b.as_str()));
        assert_ne!(a, Atom::new("span"));
        assert_eq!(a, "div");
        assert_eq!(Atom::new(""), Atom::new(""));
    }
}
