//! `DOMException` objects (Web IDL §3.14.1) and the mapping of DOM errors
//! to thrown values.

use vjs::{Agent, JsResult, Object, Throw, Value};

use super::generated::ids;
use super::platform::{self, Native};
use crate::dom::{DomError, DomException};

/// The error names with a legacy `code` (Web IDL §2.8.1, the table of
/// `DOMException` names). Every other name has code 0.
pub const LEGACY_CODES: &[(&str, u16)] = &[
    ("IndexSizeError", 1),
    ("HierarchyRequestError", 3),
    ("WrongDocumentError", 4),
    ("InvalidCharacterError", 5),
    ("NoModificationAllowedError", 7),
    ("NotFoundError", 8),
    ("NotSupportedError", 9),
    ("InUseAttributeError", 10),
    ("InvalidStateError", 11),
    ("SyntaxError", 12),
    ("InvalidModificationError", 13),
    ("NamespaceError", 14),
    ("InvalidAccessError", 15),
    ("TypeMismatchError", 17),
    ("SecurityError", 18),
    ("NetworkError", 19),
    ("AbortError", 20),
    ("URLMismatchError", 21),
    ("QuotaExceededError", 22),
    ("TimeoutError", 23),
    ("InvalidNodeTypeError", 24),
    ("DataCloneError", 25),
];

/// The legacy code of an error name.
pub fn legacy_code(name: &str) -> u16 {
    LEGACY_CODES
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(0, |(_, c)| *c)
}

/// A new `DOMException` of the current realm. Without a window realm it
/// falls back to an `Error` with the same name and message.
pub fn create(agent: &mut Agent, name: &str, message: &str) -> Object {
    let Some(state) = super::current(agent) else {
        let e = agent.create_error(vjs::agent::ErrorKind::Error, message);
        let key = agent.heap.key_from_str("name");
        let v = Value::String(agent.heap.alloc_str(name));
        agent.heap.set_own(
            e,
            key,
            vjs::object::Attributes::data(true, false, true),
            vjs::object::Slot::Data(v),
        );
        return e;
    };
    platform::create(
        agent,
        &state,
        ids::DOMException,
        Native::Exception {
            name: name.to_string(),
            message: message.to_string(),
        },
        None,
    )
}

/// The default message of a DOM error.
fn message(e: DomException) -> &'static str {
    match e {
        DomException::IndexSizeError => "The index is not in the allowed range.",
        DomException::HierarchyRequestError => "The operation would yield an incorrect node tree.",
        DomException::WrongDocumentError => "The object is in the wrong document.",
        DomException::InvalidCharacterError => "The string contains invalid characters.",
        DomException::NoModificationAllowedError => "The object can not be modified.",
        DomException::NotFoundError => "The object can not be found here.",
        DomException::NotSupportedError => "The operation is not supported.",
        DomException::InUseAttributeError => "The attribute is in use by another element.",
        DomException::InvalidStateError => "The object is in an invalid state.",
        DomException::SyntaxError => "The string did not match the expected pattern.",
        DomException::NamespaceError => "The operation is not allowed by Namespaces in XML.",
        DomException::InvalidNodeTypeError => {
            "The supplied node is incorrect or has an incorrect ancestor for this operation."
        }
        DomException::AbortError => "The operation was aborted.",
        DomException::TimeoutError => "The operation timed out.",
    }
}

/// The thrown `DOMException` for `e`.
pub fn throw(agent: &mut Agent, e: DomException) -> Throw {
    Throw(Value::Object(create(agent, e.name(), message(e))))
}

/// `Err` with the `DOMException` for `e`.
pub fn dom<T>(agent: &mut Agent, e: DomException) -> JsResult<T> {
    Err(throw(agent, e))
}

/// `Err` with the value a DOM error stands for.
pub fn dom_error<T>(agent: &mut Agent, e: DomError) -> JsResult<T> {
    match e {
        DomError::Exception(e) => dom(agent, e),
        DomError::Type(message) => agent.type_error(&message),
        DomError::Thrown(r) => Err(Throw(super::host_value(agent, r))),
    }
}
