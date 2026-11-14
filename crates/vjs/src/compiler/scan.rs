//! Static semantics the compiler needs from the AST: BoundNames,
//! LexicallyScopedDeclarations and the var-scoped function declarations.

use crate::ast::{
    ClassElementKind, Function, Params, Pattern, PatternKind, Stmt, StmtKind, VarKind,
};

/// BoundNames of a binding pattern, in source order.
pub(crate) fn bound_names(p: &Pattern, out: &mut Vec<String>) {
    match &p.kind {
        PatternKind::Ident(n) => out.push(n.clone()),
        PatternKind::Expr(_) => {}
        PatternKind::Array { elements, rest } => {
            for e in elements.iter().flatten() {
                bound_names(&e.target, out);
            }
            if let Some(r) = rest {
                bound_names(r, out);
            }
        }
        PatternKind::Object { properties, rest } => {
            for prop in properties {
                bound_names(&prop.value.target, out);
            }
            if let Some(r) = rest {
                bound_names(r, out);
            }
        }
    }
}

/// BoundNames of a formal parameter list.
pub(crate) fn param_names(params: &Params) -> Vec<String> {
    let mut out = Vec::new();
    for p in &params.items {
        bound_names(&p.target, &mut out);
    }
    if let Some(r) = &params.rest {
        bound_names(r, &mut out);
    }
    out
}

/// A lexically scoped declaration of a statement list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LexKind {
    Let,
    Const,
    Class,
    /// A function declared in a block (or a function in a `switch`).
    Function,
}

/// LexicallyScopedDeclarations of a block-like statement list (functions
/// count as lexical): `(name, kind)` in source order.
pub(crate) fn lexical_declarations(stmts: &[Stmt]) -> Vec<(String, LexKind)> {
    let mut out = Vec::new();
    for s in stmts {
        lexical_of(s, &mut out, true);
    }
    out
}

/// LexicallyScopedDeclarations of a function body or script top level:
/// functions there are var-scoped and not reported.
pub(crate) fn top_lexical_declarations(stmts: &[Stmt]) -> Vec<(String, LexKind)> {
    let mut out = Vec::new();
    for s in stmts {
        lexical_of(s, &mut out, false);
    }
    out
}

fn lexical_of(s: &Stmt, out: &mut Vec<(String, LexKind)>, functions: bool) {
    match &s.kind {
        StmtKind::Var(decl) if decl.kind != VarKind::Var => {
            let kind = if decl.kind == VarKind::Let {
                LexKind::Let
            } else {
                LexKind::Const
            };
            for d in &decl.declarations {
                let mut names = Vec::new();
                bound_names(&d.target, &mut names);
                out.extend(names.into_iter().map(|n| (n, kind)));
            }
        }
        StmtKind::Class(c) => {
            if let Some(n) = &c.name {
                out.push((n.name.clone(), LexKind::Class));
            }
        }
        StmtKind::Function(f) if functions => {
            if let Some(n) = &f.name {
                out.push((n.name.clone(), LexKind::Function));
            }
        }
        StmtKind::Labeled { body, .. } => lexical_of(body, out, functions),
        _ => {}
    }
}

/// The function declarations of a statement list that must be created when
/// the scope is entered: direct children and labelled functions.
pub(crate) fn hoisted_functions(stmts: &[Stmt]) -> Vec<&Function> {
    let mut out = Vec::new();
    for s in stmts {
        let mut s = s;
        while let StmtKind::Labeled { body, .. } = &s.kind {
            s = body;
        }
        if let StmtKind::Function(f) = &s.kind {
            out.push(&**f);
        }
    }
    out
}

/// functionsToInitialize: the last declaration of each name wins and the
/// list keeps the order of those last declarations.
pub(crate) fn functions_to_initialize<'a>(functions: &[&'a Function]) -> Vec<&'a Function> {
    let mut out: Vec<&Function> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for f in functions.iter().rev() {
        let Some(n) = &f.name else { continue };
        if seen.contains(&n.name.as_str()) {
            continue;
        }
        seen.push(&n.name);
        out.insert(0, f);
    }
    out
}

/// Whether the statement list declares `using` (`Some(true)` if any is
/// `await using`).
pub(crate) fn using_kind(stmts: &[Stmt]) -> Option<bool> {
    let mut found = None;
    for s in stmts {
        if let StmtKind::Var(decl) = &s.kind {
            match decl.kind {
                VarKind::Using => {
                    found.get_or_insert(false);
                }
                VarKind::AwaitUsing => found = Some(true),
                _ => {}
            }
        }
    }
    found
}

/// ContainsExpression of a parameter list, approximated by "is not a
/// simple parameter list": both only differ for patterns without
/// initializers or computed keys, where the separate environment is not
/// observable.
pub(crate) fn has_parameter_expressions(params: &Params) -> bool {
    !params.is_simple
}

/// Private names declared by the elements of a class body (`#x` once per
/// name, accessor pairs share one).
pub(crate) fn class_private_names(elements: &[crate::ast::ClassElement]) -> Vec<String> {
    use crate::ast::PropertyKey;
    let mut out: Vec<String> = Vec::new();
    for e in elements {
        let key = match &e.kind {
            ClassElementKind::Method { key, .. } => key,
            ClassElementKind::Field { key, .. } => key,
            ClassElementKind::StaticBlock { .. } => continue,
        };
        if let PropertyKey::Private(n) = key {
            let n = format!("#{n}");
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// ExpectedArgumentCount.
pub(crate) fn expected_argument_count(params: &Params) -> u32 {
    let mut n = 0;
    for p in &params.items {
        if p.default.is_some() {
            break;
        }
        n += 1;
    }
    n
}
