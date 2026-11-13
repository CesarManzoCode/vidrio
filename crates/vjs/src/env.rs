//! Storage of environment records: a chain of slot vectors. `None` is an
//! uninitialized binding (temporal dead zone). The binding semantics live
//! in the compiler and interpreter; mapped `arguments` objects and module
//! namespaces read and write these slots directly.

use crate::gc::{Env, Trace, Tracer};
use crate::value::Value;

pub struct EnvData {
    pub outer: Option<Env>,
    pub slots: Vec<Option<Value>>,
}

impl Trace for EnvData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(outer) = self.outer {
            tracer.env(outer);
        }
        for v in self.slots.iter().flatten() {
            tracer.value(*v);
        }
    }
}
