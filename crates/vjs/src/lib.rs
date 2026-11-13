//! vjs: JavaScript engine (lexer, parser, bytecode compiler, interpreter, GC, built-ins).

pub mod agent;
pub mod ast;
pub mod bigint;
pub mod buffer;
pub mod completion;
pub mod env;
pub mod exotic;
pub mod function;
pub mod gc;
pub mod generated;
pub mod internal;
pub mod lexer;
pub mod number;
pub mod object;
pub mod ops;
pub mod parser;
pub mod proxy;
pub mod realm;
pub mod shape;
pub mod string;
pub mod unicode;
pub mod value;
pub mod weak;

pub use agent::Agent;
pub use ast::SyntaxError;
pub use completion::{Completion, JsResult, Throw};
pub use gc::{Gc, Heap, JsString, Object, Symbol};
pub use parser::{
    DynamicFunctionKind, EvalContext, ParseOptions, RegExpValidator, parse_dynamic_function,
    parse_module, parse_script,
};
pub use value::{PropertyKey, Value};
