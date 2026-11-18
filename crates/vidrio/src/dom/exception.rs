//! `DOMException` names (WebIDL §2.8.1) and the errors DOM algorithms
//! return to their callers.

/// The `DOMException` names the DOM algorithms throw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DomException {
    IndexSizeError,
    HierarchyRequestError,
    WrongDocumentError,
    InvalidCharacterError,
    NoModificationAllowedError,
    NotFoundError,
    NotSupportedError,
    InUseAttributeError,
    InvalidStateError,
    SyntaxError,
    NamespaceError,
    InvalidNodeTypeError,
    AbortError,
    TimeoutError,
    SecurityError,
    DataCloneError,
}

impl DomException {
    /// The exception's name.
    pub fn name(self) -> &'static str {
        match self {
            DomException::IndexSizeError => "IndexSizeError",
            DomException::HierarchyRequestError => "HierarchyRequestError",
            DomException::WrongDocumentError => "WrongDocumentError",
            DomException::InvalidCharacterError => "InvalidCharacterError",
            DomException::NoModificationAllowedError => "NoModificationAllowedError",
            DomException::NotFoundError => "NotFoundError",
            DomException::NotSupportedError => "NotSupportedError",
            DomException::InUseAttributeError => "InUseAttributeError",
            DomException::InvalidStateError => "InvalidStateError",
            DomException::SyntaxError => "SyntaxError",
            DomException::NamespaceError => "NamespaceError",
            DomException::InvalidNodeTypeError => "InvalidNodeTypeError",
            DomException::AbortError => "AbortError",
            DomException::TimeoutError => "TimeoutError",
            DomException::SecurityError => "SecurityError",
            DomException::DataCloneError => "DataCloneError",
        }
    }

    /// The legacy `code` of the exception (WebIDL §2.8.1, table of error
    /// names); 0 for names without one.
    pub fn code(self) -> u16 {
        match self {
            DomException::IndexSizeError => 1,
            DomException::HierarchyRequestError => 3,
            DomException::WrongDocumentError => 4,
            DomException::InvalidCharacterError => 5,
            DomException::NoModificationAllowedError => 7,
            DomException::NotFoundError => 8,
            DomException::NotSupportedError => 9,
            DomException::InUseAttributeError => 10,
            DomException::InvalidStateError => 11,
            DomException::SyntaxError => 12,
            DomException::NamespaceError => 14,
            DomException::InvalidNodeTypeError => 24,
            DomException::AbortError => 20,
            DomException::TimeoutError => 23,
            DomException::SecurityError => 18,
            DomException::DataCloneError => 25,
        }
    }
}

/// A value owned by the embedder (a JS value in vidrio): callbacks, abort
/// reasons, `CustomEvent.detail`, thrown exceptions. The DOM only stores
/// and compares it; the bindings of k3-09 map it to the value and keep it
/// alive for the garbage collector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HostRef(pub u64);

/// What a DOM algorithm that may call into script, or throw something other
/// than a `DOMException`, returns on failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomError {
    Exception(DomException),
    /// A `TypeError` with its message.
    Type(String),
    /// An exception thrown by a callback, rethrown as is.
    Thrown(HostRef),
}

impl From<DomException> for DomError {
    fn from(e: DomException) -> DomError {
        DomError::Exception(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_codes() {
        assert_eq!(
            DomException::HierarchyRequestError.name(),
            "HierarchyRequestError"
        );
        assert_eq!(DomException::HierarchyRequestError.code(), 3);
        assert_eq!(DomException::AbortError.code(), 20);
        assert_eq!(DomException::TimeoutError.code(), 23);
        assert_eq!(DomException::SecurityError.code(), 18);
        assert_eq!(DomException::DataCloneError.code(), 25);
        assert_eq!(DomException::DataCloneError.name(), "DataCloneError");
        assert_eq!(
            DomError::from(DomException::SyntaxError),
            DomError::Exception(DomException::SyntaxError)
        );
    }
}
