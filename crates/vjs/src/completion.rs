//! Completion Records (ECMA-262 §6.2.4).
//!
//! Abstract operations return [`JsResult`]: `Ok` is a normal completion and
//! `Err(Throw)` a throw completion. [`Completion`] is the full record with
//! `break`, `continue` and `return`, used by statement evaluation.

use crate::gc::{Atom, Trace, Tracer};
use crate::value::Value;

/// The `[[Value]]` of a throw completion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Throw(pub Value);

impl Throw {
    pub fn value(self) -> Value {
        self.0
    }
}

impl Trace for Throw {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.value(self.0);
    }
}

/// Normal completion with a `T`, or a throw completion.
pub type JsResult<T> = Result<T, Throw>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionType {
    Normal,
    Break,
    Continue,
    Return,
    Throw,
}

/// A Completion Record. `value` is `None` for the `empty` value and
/// `target` is the label of `break`/`continue` (`None` for `empty`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Completion {
    pub kind: CompletionType,
    pub value: Option<Value>,
    pub target: Option<Atom>,
}

impl Completion {
    pub fn normal(value: Value) -> Completion {
        Completion {
            kind: CompletionType::Normal,
            value: Some(value),
            target: None,
        }
    }

    pub fn empty() -> Completion {
        Completion {
            kind: CompletionType::Normal,
            value: None,
            target: None,
        }
    }

    pub fn throw(value: Value) -> Completion {
        Completion {
            kind: CompletionType::Throw,
            value: Some(value),
            target: None,
        }
    }

    pub fn return_(value: Value) -> Completion {
        Completion {
            kind: CompletionType::Return,
            value: Some(value),
            target: None,
        }
    }

    pub fn break_(target: Option<Atom>) -> Completion {
        Completion {
            kind: CompletionType::Break,
            value: None,
            target,
        }
    }

    pub fn continue_(target: Option<Atom>) -> Completion {
        Completion {
            kind: CompletionType::Continue,
            value: None,
            target,
        }
    }

    pub fn is_abrupt(self) -> bool {
        self.kind != CompletionType::Normal
    }

    /// UpdateEmpty(completionRecord, value).
    pub fn update_empty(self, value: Option<Value>) -> Completion {
        if self.value.is_some() {
            return self;
        }
        Completion { value, ..self }
    }

    /// Converts a `JsResult` into a normal or throw completion.
    pub fn from_result(result: JsResult<Value>) -> Completion {
        match result {
            Ok(v) => Completion::normal(v),
            Err(Throw(v)) => Completion::throw(v),
        }
    }
}

impl Trace for Completion {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(v) = self.value {
            tracer.value(v);
        }
        if let Some(a) = self.target {
            tracer.atom(a);
        }
    }
}
