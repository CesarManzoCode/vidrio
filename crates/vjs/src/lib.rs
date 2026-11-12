//! vjs: JavaScript engine (lexer, parser, bytecode compiler, interpreter, GC, built-ins).

pub mod ast;
pub mod generated;
pub mod lexer;
pub mod parser;
pub mod unicode;

pub use ast::SyntaxError;
pub use parser::{
    DynamicFunctionKind, EvalContext, ParseOptions, RegExpValidator, parse_dynamic_function,
    parse_module, parse_script,
};
