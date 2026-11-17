//! Environment Records at run time: dynamic identifier resolution
//! (ResolveBinding through declarative, object and global records), the
//! hidden `this`/`new.target`/function bindings, and
//! GlobalDeclarationInstantiation and EvalDeclarationInstantiation.

use std::rc::Rc;

use crate::agent::Agent;
use crate::bytecode::{Code, Declarations, Hidden};
use crate::completion::JsResult;
use crate::env::{BIND_CONST, BIND_IMPORT, BIND_SLOPPY_CONST, EnvKind};
use crate::gc::{Atom, Env, Object};
use crate::internal;
use crate::object::PropertyDescriptor;
use crate::ops;
use crate::value::{PropertyKey, Value};

/// A resolved binding.
#[derive(Clone, Copy, Debug)]
pub enum Found {
    /// Slot of a declarative environment.
    Slot(Env, u32),
    /// A deletable binding added by a sloppy direct `eval`.
    EvalVar(Env, usize),
    /// A binding of the declarative part of the global environment.
    GlobalLexical(Env, u32),
    /// A property of a binding object: `with` object or global object.
    Object(Object, bool),
    Unresolvable,
}

/// The property key of an identifier.
pub(crate) fn name_key(agent: &mut Agent, name: Atom) -> PropertyKey {
    let units = agent.heap.atom_units(name).to_vec();
    agent.heap.key_from_units(&units)
}

fn is_private_name(agent: &Agent, name: Atom) -> bool {
    agent.heap.atom_units(name).first() == Some(&u16::from(b'#'))
        || agent.heap.atom_units(name).first() == Some(&u16::from(b'%'))
}

pub(crate) fn name_string(agent: &Agent, name: Atom) -> String {
    agent.heap.atom_to_rust(name)
}

/// ResolveBinding(name, env).
pub fn lookup(agent: &mut Agent, env: Env, name: Atom) -> JsResult<Found> {
    let skip_objects = is_private_name(agent, name);
    let mut cur = Some(env);
    while let Some(e) = cur {
        enum Next {
            Found(Found),
            Object(Object, bool),
            Global(Object),
            Continue,
        }
        let data = agent.heap.env(e);
        let next = match &data.kind {
            EnvKind::Declarative(names) => {
                if let Some(i) = names.as_ref().and_then(|n| n.find(name)) {
                    Next::Found(Found::Slot(e, i as u32))
                } else if let Some(i) = data
                    .eval_vars
                    .as_ref()
                    .and_then(|v| v.iter().position(|(a, _)| *a == name))
                {
                    Next::Found(Found::EvalVar(e, i))
                } else {
                    Next::Continue
                }
            }
            EnvKind::Object { object, with } => Next::Object(*object, *with),
            EnvKind::Global(g) => match g.lexical.get(&name) {
                Some(&slot) => Next::Found(Found::GlobalLexical(e, slot)),
                None => Next::Global(g.object),
            },
        };
        let outer = data.outer;
        match next {
            Next::Found(f) => return Ok(f),
            Next::Object(o, with) if !skip_objects => {
                let key = name_key(agent, name);
                if ops::has_property(agent, o, key)? {
                    if !with {
                        return Ok(Found::Object(o, false));
                    }
                    let unscopables_key = PropertyKey::Symbol(agent.symbols.unscopables);
                    let unscopables = ops::get(agent, o, unscopables_key)?;
                    let blocked = match unscopables {
                        Value::Object(u) => {
                            let v = ops::get(agent, u, key)?;
                            ops::to_boolean(agent, v)
                        }
                        _ => false,
                    };
                    if !blocked {
                        return Ok(Found::Object(o, true));
                    }
                }
            }
            Next::Global(o) => {
                if !skip_objects {
                    let key = name_key(agent, name);
                    if ops::has_property(agent, o, key)? {
                        return Ok(Found::Object(o, false));
                    }
                }
                return Ok(Found::Unresolvable);
            }
            _ => {}
        }
        cur = outer;
    }
    Ok(Found::Unresolvable)
}

/// The base of a Reference resolved by `ResolveRef`: the binding object,
/// the declarative Environment Record (its arena index as a number) or
/// `undefined`. The record stays reachable from the running environment
/// until the matching `GetRef`/`SetRef`.
pub(crate) fn reference_base(found: Found) -> Value {
    match found {
        Found::Object(o, _) => Value::Object(o),
        Found::Slot(e, _) | Found::EvalVar(e, _) | Found::GlobalLexical(e, _) => {
            Value::Number(f64::from(e.index()))
        }
        Found::Unresolvable => Value::Undefined,
    }
}

/// The binding of `name` in the base of a Reference built by
/// `reference_base`: PutValue and GetValue use that record, not a new
/// resolution, even if evaluating the right-hand side created a closer
/// binding (direct `eval`) or a `with` object gained the property.
pub(crate) fn reference_binding(agent: &Agent, base: Value, name: Atom) -> Found {
    let e = match base {
        Value::Object(o) => return Found::Object(o, false),
        Value::Number(n) => Env::from_index(n as u32),
        _ => return Found::Unresolvable,
    };
    let data = agent.heap.env(e);
    match &data.kind {
        EnvKind::Declarative(names) => {
            if let Some(i) = names.as_ref().and_then(|n| n.find(name)) {
                Found::Slot(e, i as u32)
            } else if let Some(i) = data
                .eval_vars
                .as_ref()
                .and_then(|v| v.iter().position(|(a, _)| *a == name))
            {
                Found::EvalVar(e, i)
            } else {
                Found::Unresolvable
            }
        }
        EnvKind::Global(g) => match g.lexical.get(&name) {
            Some(&slot) => Found::GlobalLexical(e, slot),
            None => Found::Unresolvable,
        },
        EnvKind::Object { object, .. } => Found::Object(*object, false),
    }
}

fn tdz_error<T>(agent: &mut Agent, name: Atom) -> JsResult<T> {
    let n = name_string(agent, name);
    agent.reference_error(&format!("cannot access '{n}' before initialization"))
}

fn not_defined<T>(agent: &mut Agent, name: Atom) -> JsResult<T> {
    let n = name_string(agent, name);
    agent.reference_error(&format!("{n} is not defined"))
}

/// The value of a resolved binding (GetBindingValue).
pub fn get_binding_value(
    agent: &mut Agent,
    found: Found,
    name: Atom,
    strict: bool,
) -> JsResult<Value> {
    match found {
        Found::Slot(e, i) | Found::GlobalLexical(e, i) => {
            // An import binding reads the binding it points to.
            let (e, i) = agent.heap.env(e).import_target(i).unwrap_or((e, i));
            let slot = agent.heap.env(e).slots[i as usize];
            match slot {
                Some(v) => Ok(v),
                None => tdz_error(agent, name),
            }
        }
        Found::EvalVar(e, i) => Ok(agent
            .heap
            .env(e)
            .eval_vars
            .as_ref()
            .and_then(|v| v.get(i).map(|(_, v)| *v))
            .unwrap_or(Value::Undefined)),
        Found::Object(o, _) => {
            let key = name_key(agent, name);
            if !ops::has_property(agent, o, key)? {
                if strict {
                    return not_defined(agent, name);
                }
                return Ok(Value::Undefined);
            }
            ops::get(agent, o, key)
        }
        Found::Unresolvable => not_defined(agent, name),
    }
}

/// SetMutableBinding on a resolved binding (PutValue).
pub fn set_binding(
    agent: &mut Agent,
    found: Found,
    name: Atom,
    value: Value,
    strict: bool,
) -> JsResult<()> {
    match found {
        Found::Slot(e, i) => {
            let flags = match &agent.heap.env(e).kind {
                EnvKind::Declarative(Some(n)) => n.flags.get(i as usize).copied().unwrap_or(0),
                _ => 0,
            };
            set_slot(agent, e, i, flags, name, value, strict)
        }
        Found::GlobalLexical(e, i) => {
            let flags = match &agent.heap.env(e).kind {
                EnvKind::Global(g) => g.flags.get(i as usize).copied().unwrap_or(0),
                _ => 0,
            };
            set_slot(agent, e, i, flags, name, value, strict)
        }
        Found::EvalVar(e, i) => {
            if let Some(vars) = agent.heap.env_mut(e).eval_vars.as_mut() {
                if let Some(entry) = vars.get_mut(i) {
                    entry.1 = value;
                }
            }
            Ok(())
        }
        Found::Object(o, _) => {
            let key = name_key(agent, name);
            let still_exists = ops::has_property(agent, o, key)?;
            if !still_exists && strict {
                return not_defined(agent, name);
            }
            ops::set(agent, o, key, value, strict)
        }
        Found::Unresolvable => {
            if strict {
                return not_defined(agent, name);
            }
            let global = agent.global_object();
            let key = name_key(agent, name);
            ops::set(agent, global, key, value, false)
        }
    }
}

fn set_slot(
    agent: &mut Agent,
    e: Env,
    i: u32,
    flags: u8,
    name: Atom,
    value: Value,
    strict: bool,
) -> JsResult<()> {
    if agent.heap.env(e).slots[i as usize].is_none() {
        return tdz_error(agent, name);
    }
    if flags & (BIND_CONST | BIND_IMPORT) != 0 {
        return agent.type_error("assignment to constant variable");
    }
    if flags & BIND_SLOPPY_CONST != 0 {
        if strict {
            return agent.type_error("assignment to constant variable");
        }
        return Ok(());
    }
    agent.heap.env_mut(e).slots[i as usize] = Some(value);
    Ok(())
}

/// InitializeBinding on a binding found by name (global lexical
/// declarations and other dynamically reached declarations).
pub fn initialize_binding(agent: &mut Agent, env: Env, name: Atom, value: Value) -> JsResult<()> {
    match lookup(agent, env, name)? {
        Found::Slot(e, i) | Found::GlobalLexical(e, i) => {
            agent.heap.env_mut(e).slots[i as usize] = Some(value);
            Ok(())
        }
        found => set_binding(agent, found, name, value, false),
    }
}

/// The `this` value seen from `env` (GetThisEnvironment().GetThisBinding()).
pub fn hidden_value(agent: &mut Agent, env: Env, h: Hidden) -> JsResult<Value> {
    let mut cur = Some(env);
    while let Some(e) = cur {
        let data = agent.heap.env(e);
        match &data.kind {
            EnvKind::Declarative(Some(names)) => {
                let slot = match h {
                    Hidden::This => names.this_slot,
                    Hidden::NewTarget => names.new_target_slot,
                    Hidden::Function => names.function_slot,
                };
                if let Some(slot) = slot {
                    let value = data.slots[slot as usize];
                    return match value {
                        Some(v) => Ok(v),
                        None => agent.reference_error(
                            "must call super constructor in derived class before accessing 'this'",
                        ),
                    };
                }
            }
            EnvKind::Global(g) => {
                return Ok(match h {
                    Hidden::This => g.this,
                    _ => Value::Undefined,
                });
            }
            _ => {}
        }
        cur = data.outer;
    }
    Ok(Value::Undefined)
}

/// BindThisValue on the nearest environment with a hidden `this`.
pub fn bind_this_dynamic(agent: &mut Agent, env: Env, value: Value) -> JsResult<()> {
    let mut cur = Some(env);
    while let Some(e) = cur {
        let data = agent.heap.env(e);
        if let EnvKind::Declarative(Some(names)) = &data.kind {
            if let Some(slot) = names.this_slot {
                return bind_this_slot(agent, e, slot, value);
            }
        }
        cur = data.outer;
    }
    agent.reference_error("'super' keyword unexpected here")
}

pub fn bind_this_slot(agent: &mut Agent, env: Env, slot: u32, value: Value) -> JsResult<()> {
    if agent.heap.env(env).slots[slot as usize].is_some() {
        return agent.reference_error("super constructor may only be called once");
    }
    agent.heap.env_mut(env).slots[slot as usize] = Some(value);
    Ok(())
}

/// The bound `this` of a derived constructor's environment.
pub(crate) fn hidden_this_of(agent: &Agent, env: Option<Env>) -> Option<Value> {
    let e = env?;
    let data = agent.heap.env(e);
    let EnvKind::Declarative(Some(names)) = &data.kind else {
        return None;
    };
    data.slots[names.this_slot? as usize]
}

/// `delete name`.
pub fn delete_binding(agent: &mut Agent, env: Env, name: Atom) -> JsResult<bool> {
    match lookup(agent, env, name)? {
        Found::Slot(..) | Found::GlobalLexical(..) => Ok(false),
        Found::EvalVar(e, i) => {
            if let Some(vars) = agent.heap.env_mut(e).eval_vars.as_mut() {
                vars.remove(i);
            }
            Ok(true)
        }
        Found::Object(o, _) => {
            let key = name_key(agent, name);
            internal::delete(agent, o, key)
        }
        Found::Unresolvable => Ok(true),
    }
}

// ----- global declarations -----

fn global_parts(agent: &Agent, env: Env) -> Object {
    match &agent.heap.env(env).kind {
        EnvKind::Global(g) => g.object,
        _ => panic!("not a global environment"),
    }
}

fn has_lexical(agent: &Agent, env: Env, name: Atom) -> bool {
    match &agent.heap.env(env).kind {
        EnvKind::Global(g) => g.lexical.contains_key(&name),
        _ => false,
    }
}

fn has_var_name(agent: &Agent, env: Env, name: Atom) -> bool {
    match &agent.heap.env(env).kind {
        EnvKind::Global(g) => g.var_names.contains(&name),
        _ => false,
    }
}

fn add_var_name(agent: &mut Agent, env: Env, name: Atom) {
    if let EnvKind::Global(g) = &mut agent.heap.env_mut(env).kind {
        if !g.var_names.contains(&name) {
            g.var_names.push(name);
        }
    }
}

/// CanDeclareGlobalFunction.
fn can_declare_global_function(
    agent: &mut Agent,
    global: Object,
    key: PropertyKey,
) -> JsResult<bool> {
    match internal::get_own_property(agent, global, key)? {
        None => internal::is_extensible(agent, global),
        Some(d) => Ok(d.configurable == Some(true)
            || (d.is_data() && d.writable == Some(true) && d.enumerable == Some(true))),
    }
}

/// CanDeclareGlobalVar.
fn can_declare_global_var(agent: &mut Agent, global: Object, key: PropertyKey) -> JsResult<bool> {
    if ops::has_own_property(agent, global, key)? {
        return Ok(true);
    }
    internal::is_extensible(agent, global)
}

/// CreateGlobalFunctionBinding.
fn create_global_function_binding(
    agent: &mut Agent,
    env: Env,
    name: Atom,
    f: Value,
    deletable: bool,
) -> JsResult<()> {
    let global = global_parts(agent, env);
    let key = name_key(agent, name);
    let existing = internal::get_own_property(agent, global, key)?;
    let desc = match existing {
        None => PropertyDescriptor::data(f, true, true, deletable),
        Some(d) if d.configurable == Some(true) => {
            PropertyDescriptor::data(f, true, true, deletable)
        }
        Some(_) => PropertyDescriptor::value_only(f),
    };
    ops::define_property_or_throw(agent, global, key, desc)?;
    ops::set(agent, global, key, f, false)?;
    add_var_name(agent, env, name);
    Ok(())
}

/// CreateGlobalVarBinding.
fn create_global_var_binding(
    agent: &mut Agent,
    env: Env,
    name: Atom,
    deletable: bool,
) -> JsResult<()> {
    let global = global_parts(agent, env);
    let key = name_key(agent, name);
    let has_own = ops::has_own_property(agent, global, key)?;
    let extensible = internal::is_extensible(agent, global)?;
    if !has_own && extensible {
        ops::define_property_or_throw(
            agent,
            global,
            key,
            PropertyDescriptor::data(Value::Undefined, true, true, deletable),
        )?;
    }
    add_var_name(agent, env, name);
    Ok(())
}

/// GlobalDeclarationInstantiation(script, env).
pub fn global_declaration_instantiation(
    agent: &mut Agent,
    env: Env,
    code: &Rc<Code>,
    decls: &Declarations,
) -> JsResult<Vec<(u32, bool)>> {
    let global = global_parts(agent, env);
    for &(name, _) in &decls.lexical {
        if has_var_name(agent, env, name) || has_lexical(agent, env, name) {
            let n = name_string(agent, name);
            return agent.syntax_error(&format!("identifier '{n}' has already been declared"));
        }
        let key = name_key(agent, name);
        if let Some(d) = internal::get_own_property(agent, global, key)? {
            if d.configurable != Some(true) {
                let n = name_string(agent, name);
                return agent.syntax_error(&format!("identifier '{n}' has already been declared"));
            }
        }
    }
    let var_like: Vec<Atom> = decls
        .functions
        .iter()
        .map(|(n, _)| *n)
        .chain(decls.var_names.iter().copied())
        .collect();
    for &name in &var_like {
        if has_lexical(agent, env, name) {
            let n = name_string(agent, name);
            return agent.syntax_error(&format!("identifier '{n}' has already been declared"));
        }
    }
    for &(name, _) in &decls.functions {
        let key = name_key(agent, name);
        if !can_declare_global_function(agent, global, key)? {
            let n = name_string(agent, name);
            return agent.type_error(&format!("cannot declare global function '{n}'"));
        }
    }
    for &name in &decls.var_names {
        let key = name_key(agent, name);
        if !can_declare_global_var(agent, global, key)? {
            let n = name_string(agent, name);
            return agent.type_error(&format!("cannot declare global variable '{n}'"));
        }
    }
    // Annex B.3.2.2.
    let mut declared = var_like;
    let mut enabled = Vec::new();
    for &(name, reg) in &decls.annex_b {
        let mut on = false;
        if !has_lexical(agent, env, name) {
            let key = name_key(agent, name);
            if can_declare_global_var(agent, global, key)? {
                if !declared.contains(&name) {
                    create_global_var_binding(agent, env, name, false)?;
                    declared.push(name);
                }
                on = true;
            }
        }
        enabled.push((reg, on));
    }
    for &(name, is_const) in &decls.lexical {
        let data = agent.heap.env_mut(env);
        let slot = data.slots.len() as u32;
        data.slots.push(None);
        if let EnvKind::Global(g) = &mut data.kind {
            g.lexical.insert(name, slot);
            while g.flags.len() < slot as usize {
                g.flags.push(0);
            }
            g.flags.push(if is_const { BIND_CONST } else { 0 });
        }
    }
    for &(name, idx) in &decls.functions {
        let f = super::function::make_closure(agent, &code.functions[idx as usize], env, None);
        create_global_function_binding(agent, env, name, Value::Object(f), false)?;
    }
    for &name in &decls.var_names {
        create_global_var_binding(agent, env, name, false)?;
    }
    Ok(enabled)
}

/// SetMutableBinding of `name` in the VariableEnvironment seen from `env`
/// (Annex B.3.2 evaluation of a block-level function declaration).
pub fn set_var_binding(agent: &mut Agent, env: Env, name: Atom, value: Value) -> JsResult<()> {
    let var_env = var_env_of(agent, env);
    let found = lookup(agent, var_env, name)?;
    set_binding(agent, found, name, value, false)
}

/// The VariableEnvironment seen from `env`: the nearest var scope.
fn var_env_of(agent: &Agent, env: Env) -> Env {
    let mut cur = env;
    loop {
        let data = agent.heap.env(cur);
        if data.is_var_scope() || data.is_param_scope() {
            return cur;
        }
        match data.outer {
            Some(o) => cur = o,
            None => return cur,
        }
    }
}

/// A binding of the parameter scope `env` itself (not one added by `eval`).
fn param_scope_has(agent: &Agent, env: Env, name: Atom) -> bool {
    matches!(&agent.heap.env(env).kind, EnvKind::Declarative(Some(n)) if n.find(name).is_some())
}

fn declarative_has(agent: &Agent, env: Env, name: Atom) -> bool {
    let data = agent.heap.env(env);
    if let EnvKind::Declarative(Some(names)) = &data.kind {
        if names.find(name).is_some() {
            return true;
        }
    }
    data.eval_vars
        .as_ref()
        .is_some_and(|v| v.iter().any(|(a, _)| *a == name))
}

/// EvalDeclarationInstantiation of sloppy eval code; `lex_env` is the
/// environment of the eval's lexical declarations.
pub fn eval_declaration_instantiation(
    agent: &mut Agent,
    lex_env: Env,
    code: &Rc<Code>,
    decls: &Declarations,
) -> JsResult<Vec<(u32, bool)>> {
    let outer = agent
        .heap
        .env(lex_env)
        .outer
        .expect("eval environment has an outer");
    let var_env = var_env_of(agent, outer);
    let var_like: Vec<Atom> = decls
        .functions
        .iter()
        .map(|(n, _)| *n)
        .chain(decls.var_names.iter().copied())
        .collect();
    let is_global = matches!(agent.heap.env(var_env).kind, EnvKind::Global(_));
    if is_global {
        for &name in &var_like {
            if has_lexical(agent, var_env, name) {
                let n = name_string(agent, name);
                return agent.syntax_error(&format!("identifier '{n}' has already been declared"));
            }
        }
    }
    // A parameter scope is both the VariableEnvironment and, in the
    // specification, the separate environment of the parameters that the
    // loop below checks: a `var` named like a parameter (or `arguments`)
    // is a SyntaxError.
    if agent.heap.env(var_env).is_param_scope() {
        for &name in &var_like {
            if param_scope_has(agent, var_env, name) {
                let n = name_string(agent, name);
                return agent.syntax_error(&format!("identifier '{n}' has already been declared"));
            }
        }
    }
    let mut this_env = outer;
    while this_env != var_env {
        let data = agent.heap.env(this_env);
        let simple_catch = matches!(&data.kind, EnvKind::Declarative(Some(n)) if n.simple_catch);
        if !matches!(data.kind, EnvKind::Object { .. }) {
            for &name in &var_like {
                if declarative_has(agent, this_env, name) && !simple_catch {
                    let n = name_string(agent, name);
                    return agent
                        .syntax_error(&format!("identifier '{n}' has already been declared"));
                }
            }
        }
        match agent.heap.env(this_env).outer {
            Some(o) => this_env = o,
            None => break,
        }
    }
    if is_global {
        let global = global_parts(agent, var_env);
        for &(name, _) in &decls.functions {
            let key = name_key(agent, name);
            if !can_declare_global_function(agent, global, key)? {
                let n = name_string(agent, name);
                return agent.type_error(&format!("cannot declare global function '{n}'"));
            }
        }
        for &name in &decls.var_names {
            let key = name_key(agent, name);
            if !can_declare_global_var(agent, global, key)? {
                let n = name_string(agent, name);
                return agent.type_error(&format!("cannot declare global variable '{n}'"));
            }
        }
    }
    // Annex B.3.2.3.
    let mut declared = var_like;
    let mut enabled = Vec::new();
    for &(name, reg) in &decls.annex_b {
        let mut binding_exists =
            agent.heap.env(var_env).is_param_scope() && param_scope_has(agent, var_env, name);
        let mut this_env = lex_env;
        while this_env != var_env {
            let data = agent.heap.env(this_env);
            if !matches!(data.kind, EnvKind::Object { .. })
                && declarative_has(agent, this_env, name)
            {
                binding_exists = true;
            }
            match agent.heap.env(this_env).outer {
                Some(o) => this_env = o,
                None => break,
            }
        }
        let definable = if !binding_exists && is_global {
            if has_lexical(agent, var_env, name) {
                false
            } else {
                let global = global_parts(agent, var_env);
                let key = name_key(agent, name);
                can_declare_global_var(agent, global, key)?
            }
        } else {
            true
        };
        let on = !binding_exists && definable;
        if on && !declared.contains(&name) {
            if is_global {
                create_global_var_binding(agent, var_env, name, true)?;
            } else if !declarative_has(agent, var_env, name) {
                add_eval_var(agent, var_env, name, Value::Undefined);
            }
            declared.push(name);
        }
        enabled.push((reg, on));
    }
    for &(name, idx) in &decls.functions {
        let f = super::function::make_closure(agent, &code.functions[idx as usize], lex_env, None);
        let fv = Value::Object(f);
        if is_global {
            create_global_function_binding(agent, var_env, name, fv, true)?;
        } else if declarative_has(agent, var_env, name) {
            let found = lookup(agent, var_env, name)?;
            set_binding(agent, found, name, fv, false)?;
        } else {
            add_eval_var(agent, var_env, name, fv);
        }
    }
    for &name in &decls.var_names {
        if is_global {
            create_global_var_binding(agent, var_env, name, true)?;
        } else if !declarative_has(agent, var_env, name) {
            add_eval_var(agent, var_env, name, Value::Undefined);
        }
    }
    Ok(enabled)
}

fn add_eval_var(agent: &mut Agent, env: Env, name: Atom, value: Value) {
    let data = agent.heap.env_mut(env);
    data.eval_vars
        .get_or_insert_with(|| Box::new(Vec::new()))
        .push((name, value));
}
