//! ScriptEvaluation for the host and PerformEval for `eval`.

use std::rc::Rc;

use crate::agent::Agent;
use crate::ast::SyntaxError;
use crate::bytecode::EvalSite;
use crate::compiler::{compile_eval, compile_script};
use crate::completion::JsResult;
use crate::function::NativeCall;
use crate::gc::{Env, Realm};
use crate::interp::{Frame, FrameKind, global_env, run};
use crate::parser::{EvalContext, ParseOptions, parse_script};
use crate::value::Value;

/// Throws a parse error as a SyntaxError object of the current realm.
fn throw_syntax_error<T>(agent: &mut Agent, e: &SyntaxError) -> JsResult<T> {
    agent.syntax_error(&e.message)
}

/// Parses, compiles and runs `source` as a Script in the agent's first
/// realm (ParseScript + ScriptEvaluation). Its completion value is the
/// result; a parse error is thrown as a SyntaxError.
pub fn evaluate_script(agent: &mut Agent, source: &str) -> JsResult<Value> {
    let realm = agent
        .realms()
        .first()
        .copied()
        .expect("the agent has no realm");
    evaluate_script_in(agent, realm, source)
}

/// [`evaluate_script`] in a given realm.
pub fn evaluate_script_in(agent: &mut Agent, realm: Realm, source: &str) -> JsResult<Value> {
    let options = ParseOptions::default();
    agent.push_context(crate::agent::ExecutionContext {
        realm,
        function: None,
        script_or_module: None,
    });
    let result = (|| {
        let program = match parse_script(source, &options) {
            Ok(p) => p,
            Err(e) => return throw_syntax_error(agent, &e),
        };
        let code = compile_script(agent, &program, Rc::from(source));
        let env = global_env(agent, realm);
        let mut frame = Frame::new(code, env, realm, FrameKind::Script);
        frame.this = match agent.heap.env(env).kind {
            crate::env::EnvKind::Global(ref g) => Some(g.this),
            _ => None,
        };
        run(agent, frame)
    })();
    agent.pop_context();
    result
}

/// The code and frame of an eval: `Ok(Err(v))` when the argument is not a
/// string (eval returns it unchanged).
pub(crate) fn prepare_eval(
    agent: &mut Agent,
    x: Value,
    direct: Option<(Env, &EvalSite)>,
) -> JsResult<Result<Frame, Value>> {
    let Value::String(s) = x else {
        return Ok(Err(x));
    };
    let source = agent.heap.string_to_rust(s);
    let realm = agent.current_realm();
    let (env, site) = match direct {
        Some((env, site)) => (env, site.clone()),
        None => (global_env(agent, realm), EvalSite::default()),
    };
    let options = ParseOptions {
        strict: site.strict,
        eval: Some(EvalContext {
            new_target: site.new_target,
            super_property: site.super_property,
            super_call: site.super_call,
            class_field_initializer: site.class_field_initializer,
            private_names: site.private_names.clone(),
        }),
        ..ParseOptions::default()
    };
    let program = match parse_script(&source, &options) {
        Ok(p) => p,
        Err(e) => return throw_syntax_error(agent, &e),
    };
    let code = compile_eval(agent, &program, Rc::from(source.as_str()), site);
    Ok(Ok(Frame::new(code, env, realm, FrameKind::Eval)))
}

/// `eval(x)` called indirectly (`%eval%`).
pub(crate) fn eval_function(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    match prepare_eval(agent, call.arg(0), None)? {
        Err(v) => Ok(v),
        Ok(frame) => run(agent, frame),
    }
}
