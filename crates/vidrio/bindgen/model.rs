//! Resolution of parsed IDL fragments (Web IDL §2.2–§2.5): partial
//! definitions merged into their definition, mixins copied into the
//! interfaces that include them, typedefs replaced, members not exposed in
//! `Window` dropped.

use std::collections::{HashMap, HashSet};

use super::idl::{
    Argument, Definition, DictMember, ExtAttr, ExtValue, InterfaceKind, Member, Type, TypeKind,
    find_ext, has_ext,
};

/// A member with the definition it was written in: the interface itself,
/// one of its partial definitions, or a mixin (whose name is kept so that
/// every includer shares one implementation).
#[derive(Clone, Debug, PartialEq)]
pub struct OwnedMember {
    pub owner: String,
    pub member: Member,
    pub secure_context: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Iface {
    pub name: String,
    pub kind: InterfaceKind,
    pub parent: Option<String>,
    pub ext: Vec<ExtAttr>,
    pub members: Vec<OwnedMember>,
    /// Exposed in `Window` (the only global vidrio has).
    pub exposed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dict {
    pub name: String,
    pub parent: Option<String>,
    pub members: Vec<DictMember>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CallbackDef {
    pub name: String,
    pub ext: Vec<ExtAttr>,
    pub ret: Type,
    pub args: Vec<Argument>,
}

/// What a name refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameKind {
    Interface(usize),
    Dictionary(usize),
    Enum(usize),
    Callback(usize),
}

#[derive(Debug, Default)]
pub struct Model {
    pub interfaces: Vec<Iface>,
    pub dictionaries: Vec<Dict>,
    pub enums: Vec<EnumDef>,
    pub callbacks: Vec<CallbackDef>,
    pub typedefs: HashMap<String, Type>,
    pub names: HashMap<String, NameKind>,
}

/// `Some(true)` if `[Exposed]` names `Window` or `*`, `Some(false)` if it
/// names other globals only, `None` without `[Exposed]`.
pub fn exposed_in_window(ext: &[ExtAttr]) -> Option<bool> {
    let a = find_ext(ext, "Exposed")?;
    Some(match &a.value {
        ExtValue::Wildcard => true,
        ExtValue::Ident(g) => g == "Window",
        ExtValue::List(list) => list.iter().any(|g| g == "Window"),
        _ => true,
    })
}

fn member_exposed(ext: &[ExtAttr]) -> bool {
    exposed_in_window(ext).unwrap_or(true)
}

impl Model {
    /// Builds the model from the definitions of every fragment.
    pub fn build(defs: Vec<Definition>) -> Result<Model, String> {
        let mut model = Model::default();
        let mut mixins: HashMap<
            String,
            (Vec<ExtAttr>, Vec<Member>, Vec<(Vec<ExtAttr>, Vec<Member>)>),
        > = HashMap::new();
        let mut partials: Vec<(String, Vec<ExtAttr>, Vec<Member>)> = Vec::new();
        let mut partial_dicts: Vec<(String, Vec<DictMember>)> = Vec::new();
        let mut includes: Vec<(String, String)> = Vec::new();

        for def in defs {
            match def {
                Definition::Interface {
                    ext,
                    kind: InterfaceKind::Mixin,
                    name,
                    members,
                    partial,
                    ..
                } => {
                    let entry = mixins
                        .entry(name)
                        .or_insert_with(|| (Vec::new(), Vec::new(), Vec::new()));
                    if partial {
                        entry.2.push((ext, members));
                    } else {
                        entry.0 = ext;
                        entry.1 = members;
                    }
                }
                Definition::Interface {
                    ext,
                    kind,
                    name,
                    parent,
                    members,
                    partial,
                } => {
                    if partial || model.names.contains_key(&name) {
                        // A second definition of a name is merged like a
                        // partial one.
                        partials.push((name, ext, members));
                        continue;
                    }
                    let exposed = exposed_in_window(&ext).unwrap_or(true);
                    let secure = has_ext(&ext, "SecureContext");
                    let owned = members
                        .into_iter()
                        .filter(|m| member_exposed(m.ext()))
                        .map(|m| OwnedMember {
                            owner: name.clone(),
                            secure_context: secure || has_ext(m.ext(), "SecureContext"),
                            member: m,
                        })
                        .collect();
                    model
                        .names
                        .insert(name.clone(), NameKind::Interface(model.interfaces.len()));
                    model.interfaces.push(Iface {
                        name,
                        kind,
                        parent,
                        ext,
                        members: owned,
                        exposed,
                    });
                }
                Definition::Dictionary {
                    name,
                    parent,
                    members,
                    partial,
                    ..
                } => {
                    if partial {
                        partial_dicts.push((name, members));
                        continue;
                    }
                    model
                        .names
                        .insert(name.clone(), NameKind::Dictionary(model.dictionaries.len()));
                    model.dictionaries.push(Dict {
                        name,
                        parent,
                        members,
                    });
                }
                Definition::Enum { name, values, .. } => {
                    model
                        .names
                        .insert(name.clone(), NameKind::Enum(model.enums.len()));
                    model.enums.push(EnumDef { name, values });
                }
                Definition::Typedef { ty, name, .. } => {
                    model.typedefs.insert(name, ty);
                }
                Definition::Callback {
                    ext,
                    name,
                    ret,
                    args,
                } => {
                    model
                        .names
                        .insert(name.clone(), NameKind::Callback(model.callbacks.len()));
                    model.callbacks.push(CallbackDef {
                        name,
                        ext,
                        ret,
                        args,
                    });
                }
                Definition::Includes { target, mixin } => includes.push((target, mixin)),
            }
        }

        for (name, ext, members) in partials {
            let Some(NameKind::Interface(i)) = model.names.get(&name).copied() else {
                // A partial of an interface outside the copied files.
                continue;
            };
            if exposed_in_window(&ext) == Some(false) {
                continue;
            }
            let secure = has_ext(&ext, "SecureContext")
                || has_ext(&model.interfaces[i].ext, "SecureContext");
            for m in members {
                if !member_exposed(m.ext()) {
                    continue;
                }
                let secure_context = secure || has_ext(m.ext(), "SecureContext");
                model.interfaces[i].members.push(OwnedMember {
                    owner: name.clone(),
                    member: m,
                    secure_context,
                });
            }
        }

        for (name, members) in partial_dicts {
            if let Some(NameKind::Dictionary(i)) = model.names.get(&name).copied() {
                model.dictionaries[i].members.extend(members);
            }
        }
        for d in &mut model.dictionaries {
            d.members.sort_by(|a, b| a.name.cmp(&b.name));
        }

        let mut seen = HashSet::new();
        for (target, mixin) in includes {
            if !seen.insert((target.clone(), mixin.clone())) {
                continue;
            }
            let Some(NameKind::Interface(i)) = model.names.get(&target).copied() else {
                continue;
            };
            let Some((ext, members, partial_parts)) = mixins.get(&mixin) else {
                continue;
            };
            if exposed_in_window(ext) == Some(false) {
                continue;
            }
            let secure =
                has_ext(ext, "SecureContext") || has_ext(&model.interfaces[i].ext, "SecureContext");
            let mut all: Vec<(bool, &Member)> = members.iter().map(|m| (secure, m)).collect();
            for (pext, pmembers) in partial_parts {
                if exposed_in_window(pext) == Some(false) {
                    continue;
                }
                let psecure = secure || has_ext(pext, "SecureContext");
                all.extend(pmembers.iter().map(|m| (psecure, m)));
            }
            for (secure, m) in all {
                if !member_exposed(m.ext()) {
                    continue;
                }
                model.interfaces[i].members.push(OwnedMember {
                    owner: mixin.clone(),
                    member: m.clone(),
                    secure_context: secure || has_ext(m.ext(), "SecureContext"),
                });
            }
        }

        // [LegacyUnforgeable] on an interface applies to its regular
        // attributes and operations.
        for iface in &mut model.interfaces {
            if !has_ext(&iface.ext, "LegacyUnforgeable") {
                continue;
            }
            for m in &mut iface.members {
                match &mut m.member {
                    Member::Attribute {
                        ext,
                        is_static: false,
                        ..
                    }
                    | Member::Operation {
                        ext,
                        is_static: false,
                        ..
                    } => {
                        if !has_ext(ext, "LegacyUnforgeable") {
                            ext.push(ExtAttr {
                                name: "LegacyUnforgeable".into(),
                                value: ExtValue::None,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(model)
    }

    /// The type with every typedef replaced (Web IDL §2.13.28: the
    /// nullability and annotations of the typedef reference carry over).
    pub fn resolve(&self, ty: &Type) -> Type {
        self.resolve_depth(ty, 0)
    }

    fn resolve_depth(&self, ty: &Type, depth: usize) -> Type {
        let kind = match &ty.kind {
            TypeKind::Named(n) => {
                if let (Some(t), true) = (self.typedefs.get(n), depth < 32) {
                    let mut r = self.resolve_depth(t, depth + 1);
                    r.nullable |= ty.nullable;
                    r.ext.extend(ty.ext.iter().cloned());
                    return r;
                }
                TypeKind::Named(n.clone())
            }
            TypeKind::Sequence(t) => TypeKind::Sequence(Box::new(self.resolve_depth(t, depth))),
            TypeKind::FrozenArray(t) => {
                TypeKind::FrozenArray(Box::new(self.resolve_depth(t, depth)))
            }
            TypeKind::ObservableArray(t) => {
                TypeKind::ObservableArray(Box::new(self.resolve_depth(t, depth)))
            }
            TypeKind::Promise(t) => TypeKind::Promise(Box::new(self.resolve_depth(t, depth))),
            TypeKind::Record(k, v) => TypeKind::Record(
                Box::new(self.resolve_depth(k, depth)),
                Box::new(self.resolve_depth(v, depth)),
            ),
            TypeKind::Union(members) => TypeKind::Union(
                members
                    .iter()
                    .map(|m| self.resolve_depth(m, depth))
                    .collect(),
            ),
        };
        Type {
            kind,
            nullable: ty.nullable,
            ext: ty.ext.clone(),
        }
    }

    pub fn interface(&self, name: &str) -> Option<usize> {
        match self.names.get(name) {
            Some(NameKind::Interface(i)) => Some(*i),
            _ => None,
        }
    }

    pub fn dictionary(&self, name: &str) -> Option<usize> {
        match self.names.get(name) {
            Some(NameKind::Dictionary(i)) => Some(*i),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::idl::parse;
    use super::*;

    #[test]
    fn merges_partials_mixins_and_typedefs() {
        let defs = parse(
            r#"
            [Exposed=Window] interface Node { readonly attribute long a; };
            partial interface Node { undefined b(); };
            [Exposed=Worker] partial interface Node { undefined hidden(); };
            interface mixin ParentNode { readonly attribute long children; [Exposed=Worker] undefined w(); };
            Node includes ParentNode;
            [Exposed=Worker] interface WorkerOnly {};
            typedef (Node or DOMString) NodeOrString;
            dictionary D { long z; long a; };
            partial dictionary D { long m; };
            "#,
        )
        .unwrap();
        let model = Model::build(defs).unwrap();
        let node = &model.interfaces[model.interface("Node").unwrap()];
        let names: Vec<(&str, String)> = node
            .members
            .iter()
            .map(|m| {
                let n = match &m.member {
                    Member::Attribute { name, .. } => name.clone(),
                    Member::Operation { name, .. } => name.clone().unwrap_or_default(),
                    _ => String::new(),
                };
                (m.owner.as_str(), n)
            })
            .collect();
        assert_eq!(
            names,
            vec![
                ("Node", "a".to_string()),
                ("Node", "b".to_string()),
                ("ParentNode", "children".to_string()),
            ]
        );
        assert!(!model.interfaces[model.interface("WorkerOnly").unwrap()].exposed);
        let t = model.resolve(&Type {
            kind: TypeKind::Named("NodeOrString".into()),
            nullable: true,
            ext: Vec::new(),
        });
        assert!(t.nullable);
        assert!(matches!(t.kind, TypeKind::Union(ref m) if m.len() == 2));
        let d = &model.dictionaries[model.dictionary("D").unwrap()];
        let order: Vec<&str> = d.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(order, vec!["a", "m", "z"]);
    }
}
