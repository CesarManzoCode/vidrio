//! vidrio's Web IDL bindings generator. `build.rs` includes this module:
//! it parses every `.idl` file of `crates/vidrio/idl/`, resolves the
//! definitions ([`model`]) and writes `bindings.rs` to `OUT_DIR`: the `Op`
//! enumeration (one variant per implemented member) and the static tables
//! of `src/bindings/meta.rs` that the runtime of `src/bindings/` turns
//! into interface objects, prototypes and conversions.

pub mod idl;
pub mod model;

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use idl::{
    Argument, DefaultValue, ExtAttr, ExtValue, InterfaceKind, Member, Special, Type, TypeKind,
    find_ext, has_ext,
};
use model::{Iface, Model, NameKind};

/// Reads every IDL file of `idl_dir` (sorted by name), and writes the
/// generated Rust code to `out`.
pub fn generate(idl_dir: &Path, out: &Path) -> Result<(), String> {
    let mut files: Vec<_> = std::fs::read_dir(idl_dir)
        .map_err(|e| format!("{}: {e}", idl_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "idl"))
        .collect();
    files.sort();
    let mut defs = Vec::new();
    for f in &files {
        let source = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let mut parsed = idl::parse(&source).map_err(|e| format!("{}: {e}", f.display()))?;
        defs.append(&mut parsed);
    }
    let model = Model::build(defs)?;
    let code = emit(&model);
    std::fs::write(out, code).map_err(|e| format!("{}: {e}", out.display()))
}

/// The Rust identifier for an IDL name.
fn ident(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn f64_lit(v: f64) -> String {
    if v.is_nan() {
        "f64::NAN".to_string()
    } else if v == f64::INFINITY {
        "f64::INFINITY".to_string()
    } else if v == f64::NEG_INFINITY {
        "f64::NEG_INFINITY".to_string()
    } else {
        format!("{v:?}")
    }
}

fn default_lit(d: &DefaultValue) -> String {
    match d {
        DefaultValue::Null => "DefaultValue::Null".into(),
        DefaultValue::Undefined => "DefaultValue::Undefined".into(),
        DefaultValue::Bool(b) => format!("DefaultValue::Bool({b})"),
        DefaultValue::Number(n) => format!("DefaultValue::Number({})", f64_lit(*n)),
        DefaultValue::Str(s) => format!("DefaultValue::Str({s:?})"),
        DefaultValue::EmptySequence => "DefaultValue::EmptySequence".into(),
        DefaultValue::EmptyDictionary => "DefaultValue::EmptyDictionary".into(),
    }
}

fn opt_default(d: &Option<DefaultValue>) -> String {
    match d {
        Some(d) => format!("Some({})", default_lit(d)),
        None => "None".into(),
    }
}

const BUFFERS: &[&str] = &[
    "ArrayBuffer",
    "SharedArrayBuffer",
    "DataView",
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "BigInt64Array",
    "BigUint64Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
];

/// A group of overloads with the same identifier.
struct Group<'a> {
    name: String,
    is_static: bool,
    entries: Vec<(String, &'a [Argument], &'a Type, &'a [ExtAttr], bool)>,
}

struct Emitter<'a> {
    model: &'a Model,
    ops: Vec<String>,
    op_set: HashSet<String>,
}

impl<'a> Emitter<'a> {
    fn op(&mut self, name: &str) -> String {
        let name = ident(name);
        if self.op_set.insert(name.clone()) {
            self.ops.push(name.clone());
        }
        format!("Op::{name}")
    }

    fn ty(&self, t: &Type) -> String {
        let t = self.model.resolve(t);
        self.resolved_ty(&t)
    }

    fn resolved_ty(&self, t: &Type) -> String {
        let mut s = match &t.kind {
            TypeKind::Named(n) => self.named_ty(n),
            TypeKind::Sequence(e) => format!("Ty::Sequence(&{})", self.resolved_ty(e)),
            TypeKind::FrozenArray(e) => format!("Ty::FrozenArray(&{})", self.resolved_ty(e)),
            TypeKind::ObservableArray(e) => {
                format!("Ty::ObservableArray(&{})", self.resolved_ty(e))
            }
            TypeKind::Promise(e) => format!("Ty::Promise(&{})", self.resolved_ty(e)),
            TypeKind::Record(k, v) => format!(
                "Ty::Record(&{}, &{})",
                self.resolved_ty(k),
                self.resolved_ty(v)
            ),
            TypeKind::Union(members) => {
                let parts: Vec<String> = members.iter().map(|m| self.resolved_ty(m)).collect();
                format!("Ty::Union(&[{}])", parts.join(", "))
            }
        };
        if t.nullable {
            s = format!("Ty::Nullable(&{s})");
        }
        for a in &t.ext {
            let wrapper = match a.name.as_str() {
                "EnforceRange" => "EnforceRange",
                "Clamp" => "Clamp",
                "LegacyNullToEmptyString" => "LegacyNullToEmptyString",
                "AllowShared" => "AllowShared",
                _ => continue,
            };
            s = format!("Ty::{wrapper}(&{s})");
        }
        s
    }

    fn named_ty(&self, n: &str) -> String {
        let prim = match n {
            "any" => "Ty::Any",
            "undefined" => "Ty::Undefined",
            "boolean" => "Ty::Boolean",
            "byte" => "Ty::Byte",
            "octet" => "Ty::Octet",
            "short" => "Ty::Short",
            "unsigned short" => "Ty::UnsignedShort",
            "long" => "Ty::Long",
            "unsigned long" => "Ty::UnsignedLong",
            "long long" => "Ty::LongLong",
            "unsigned long long" => "Ty::UnsignedLongLong",
            "float" => "Ty::Float",
            "unrestricted float" => "Ty::UnrestrictedFloat",
            "double" => "Ty::Double",
            "unrestricted double" => "Ty::UnrestrictedDouble",
            "bigint" => "Ty::BigInt",
            "DOMString" => "Ty::DomString",
            "ByteString" => "Ty::ByteString",
            "USVString" => "Ty::UsvString",
            "object" => "Ty::Object",
            "symbol" => "Ty::Symbol",
            _ => "",
        };
        if !prim.is_empty() {
            return prim.to_string();
        }
        if BUFFERS.contains(&n) {
            return format!("Ty::Buffer(BufferKind::{n})");
        }
        let lookup = if n == "WindowProxy" { "Window" } else { n };
        match self.model.names.get(lookup) {
            Some(NameKind::Interface(i)) => {
                if self.model.interfaces[*i].kind == InterfaceKind::CallbackInterface {
                    format!("Ty::CallbackInterface({i})")
                } else {
                    format!("Ty::Interface({i})")
                }
            }
            Some(NameKind::Dictionary(i)) => format!("Ty::Dictionary({i})"),
            Some(NameKind::Enum(i)) => format!("Ty::Enum({i})"),
            Some(NameKind::Callback(i)) => format!("Ty::Callback({i})"),
            None => format!("Ty::Unknown({n:?})"),
        }
    }

    fn is_index_type(&self, t: &Type) -> bool {
        matches!(&self.model.resolve(t).kind, TypeKind::Named(n) if n == "unsigned long")
    }

    fn args(&self, args: &[Argument]) -> String {
        let parts: Vec<String> = args
            .iter()
            .map(|a| {
                format!(
                    "Argument {{ name: {:?}, ty: {}, optional: {}, variadic: {}, default: {} }}",
                    a.name,
                    self.ty(&a.ty),
                    a.optional,
                    a.variadic,
                    opt_default(&a.default)
                )
            })
            .collect();
        format!("&[{}]", parts.join(", "))
    }

    fn operation(&mut self, g: &Group<'_>, secure: bool) -> String {
        let mut overloads = Vec::new();
        let mut length = u32::MAX;
        let mut unforgeable = false;
        let mut unscopable = false;
        let mut new_object = false;
        for (i, (op, args, ret, ext, _)) in g.entries.iter().enumerate() {
            let op = self.op(op);
            let required = args
                .iter()
                .take_while(|a| !a.optional && !a.variadic)
                .count() as u32;
            length = length.min(required);
            unforgeable |= has_ext(ext, "LegacyUnforgeable");
            unscopable |= has_ext(ext, "Unscopable");
            new_object |= has_ext(ext, "NewObject");
            overloads.push(format!(
                "Overload {{ op: {op}, index: {i}, args: {}, ret: {} }}",
                self.args(args),
                self.ty(ret)
            ));
        }
        if length == u32::MAX {
            length = 0;
        }
        let secure = secure || g.entries.iter().any(|e| e.4);
        format!(
            "Operation {{ name: {:?}, overloads: &[{}], length: {length}, is_static: {}, unforgeable: {unforgeable}, unscopable: {unscopable}, secure_context: {secure}, new_object: {new_object} }}",
            g.name,
            overloads.join(", "),
            g.is_static
        )
    }

    fn interface(&mut self, iface: &Iface) -> String {
        let undefined = Type::named("undefined");
        let no_ext: &[ExtAttr] = &[];
        let name = &iface.name;
        let iface_secure = has_ext(&iface.ext, "SecureContext");
        let mut constants = Vec::new();
        let mut attributes = Vec::new();
        let mut groups: Vec<Group<'_>> = Vec::new();
        let mut ctor = Group {
            name: name.clone(),
            is_static: false,
            entries: Vec::new(),
        };
        let mut special = [
            "None".to_string(),
            "None".to_string(),
            "None".to_string(),
            "None".to_string(),
            "None".to_string(),
            "None".to_string(),
            "None".to_string(),
        ];
        let mut iterable = "Iterable::None".to_string();
        let mut stringifier = "Stringifier::None".to_string();

        for om in &iface.members {
            let owner = &om.owner;
            match &om.member {
                Member::Const {
                    name: cname, value, ..
                } => constants.push(format!(
                    "Constant {{ name: {cname:?}, value: {} }}",
                    default_lit(value)
                )),
                Member::Attribute {
                    ext,
                    name: aname,
                    ty,
                    readonly,
                    is_static,
                    stringifier: is_stringifier,
                    ..
                } => {
                    let prefix = if *is_static { "static_" } else { "" };
                    let getter = self.op(&format!("{owner}_{prefix}get_{aname}"));
                    let setter = if *readonly {
                        "None".to_string()
                    } else {
                        format!("Some({})", self.op(&format!("{owner}_{prefix}set_{aname}")))
                    };
                    if *is_stringifier {
                        stringifier = format!("Stringifier::Attribute({getter})");
                    }
                    let put_forwards = match find_ext(ext, "PutForwards").map(|a| &a.value) {
                        Some(ExtValue::Ident(f)) => format!("Some({f:?})"),
                        _ => "None".to_string(),
                    };
                    attributes.push(format!(
                        "Attribute {{ name: {aname:?}, ty: {}, getter: {getter}, setter: {setter}, readonly: {readonly}, is_static: {is_static}, unforgeable: {}, unscopable: {}, secure_context: {}, replaceable: {}, put_forwards: {put_forwards}, same_object: {}, lenient_this: {}, lenient_setter: {} }}",
                        self.ty(ty),
                        has_ext(ext, "LegacyUnforgeable"),
                        has_ext(ext, "Unscopable"),
                        om.secure_context,
                        has_ext(ext, "Replaceable"),
                        has_ext(ext, "SameObject"),
                        has_ext(ext, "LegacyLenientThis"),
                        has_ext(ext, "LegacyLenientSetter"),
                    ));
                }
                Member::Operation {
                    ext,
                    name: oname,
                    ret,
                    args,
                    is_static,
                    special: sp,
                    stringifier: is_stringifier,
                } => {
                    if let Some(sp) = sp {
                        let indexed = args.first().is_some_and(|a| self.is_index_type(&a.ty));
                        let op = match oname {
                            Some(n) => self.op(&format!("{owner}_{n}")),
                            None => {
                                let kind = match (sp, indexed) {
                                    (Special::Getter, true) => "indexed_getter",
                                    (Special::Getter, false) => "named_getter",
                                    (Special::Setter, true) => "indexed_setter",
                                    (Special::Setter, false) => "named_setter",
                                    (Special::Deleter, _) => "named_deleter",
                                };
                                self.op(&format!("{owner}_{kind}"))
                            }
                        };
                        match (sp, indexed) {
                            (Special::Getter, true) => {
                                special[0] = format!("Some(({op}, {}))", self.ty(ret));
                                let count = self.op(&format!("{owner}_index_count"));
                                special[5] = format!("Some({count})");
                            }
                            (Special::Setter, true) => {
                                let vt = args.get(1).map_or(&undefined, |a| &a.ty);
                                special[1] = format!("Some(({op}, {}))", self.ty(vt));
                            }
                            (Special::Getter, false) => {
                                special[2] = format!("Some(({op}, {}))", self.ty(ret));
                                let names = self.op(&format!("{owner}_supported_names"));
                                special[6] = format!("Some({names})");
                            }
                            (Special::Setter, false) => {
                                let vt = args.get(1).map_or(&undefined, |a| &a.ty);
                                special[3] = format!("Some(({op}, {}))", self.ty(vt));
                            }
                            (Special::Deleter, _) => special[4] = format!("Some({op})"),
                        }
                        if oname.is_none() {
                            continue;
                        }
                    }
                    let Some(oname) = oname else {
                        if *is_stringifier {
                            let op = self.op(&format!("{owner}_stringifier"));
                            stringifier = format!("Stringifier::Operation({op})");
                        }
                        continue;
                    };
                    let prefix = if *is_static { "static_" } else { "" };
                    let op_name = format!("{owner}_{prefix}{oname}");
                    if *is_stringifier {
                        let op = self.op(&op_name);
                        stringifier = format!("Stringifier::Operation({op})");
                    }
                    let entry = (
                        op_name,
                        args.as_slice(),
                        ret,
                        ext.as_slice(),
                        om.secure_context,
                    );
                    match groups
                        .iter_mut()
                        .find(|g| g.name == *oname && g.is_static == *is_static)
                    {
                        Some(g) => g.entries.push(entry),
                        None => groups.push(Group {
                            name: oname.clone(),
                            is_static: *is_static,
                            entries: vec![entry],
                        }),
                    }
                }
                Member::Constructor { ext, args } => ctor.entries.push((
                    format!("{name}_constructor"),
                    args.as_slice(),
                    &undefined,
                    ext.as_slice(),
                    false,
                )),
                Member::Stringifier { .. } => {
                    let op = self.op(&format!("{owner}_stringifier"));
                    stringifier = format!("Stringifier::Operation({op})");
                }
                Member::Iterable {
                    key,
                    value,
                    is_async,
                    ..
                } => {
                    if *is_async {
                        continue;
                    }
                    iterable = match key {
                        None => format!("Iterable::Value({})", self.ty(value)),
                        Some(k) => {
                            let pairs = self.op(&format!("{owner}_iterable_pairs"));
                            format!(
                                "Iterable::Pair {{ key: {}, value: {}, pairs: {pairs} }}",
                                self.ty(k),
                                self.ty(value)
                            )
                        }
                    };
                }
                Member::Maplike {
                    readonly,
                    key,
                    value,
                    ..
                } => {
                    iterable = format!(
                        "Iterable::Maplike {{ key: {}, value: {}, readonly: {readonly} }}",
                        self.ty(key),
                        self.ty(value)
                    );
                }
                Member::Setlike {
                    readonly, value, ..
                } => {
                    iterable = format!(
                        "Iterable::Setlike {{ value: {}, readonly: {readonly} }}",
                        self.ty(value)
                    );
                }
            }
        }

        let operations: Vec<String> = groups
            .iter()
            .map(|g| self.operation(g, false))
            .collect::<Vec<_>>();
        let constructor = if ctor.entries.is_empty() {
            "None".to_string()
        } else {
            format!("Some(&{})", self.operation(&ctor, iface_secure))
        };
        let mut factories = Vec::new();
        for a in iface
            .ext
            .iter()
            .filter(|a| a.name == "LegacyFactoryFunction")
        {
            if let ExtValue::NamedArgs(fname, args) = &a.value {
                let g = Group {
                    name: fname.clone(),
                    is_static: false,
                    entries: vec![(
                        format!("{name}_factory_{fname}"),
                        args.as_slice(),
                        &undefined,
                        no_ext,
                        false,
                    )],
                };
                factories.push(self.operation(&g, false));
            }
        }
        let aliases: Vec<String> = match find_ext(&iface.ext, "LegacyWindowAlias").map(|a| &a.value)
        {
            Some(ExtValue::Ident(a)) => vec![format!("{a:?}")],
            Some(ExtValue::List(list)) => list.iter().map(|a| format!("{a:?}")).collect(),
            _ => Vec::new(),
        };
        let parent = match iface
            .parent
            .as_deref()
            .and_then(|p| self.model.interface(p))
        {
            Some(p) => format!("Some({p})"),
            None => "None".to_string(),
        };
        let kind = match iface.kind {
            InterfaceKind::Namespace => "Kind::Namespace",
            InterfaceKind::CallbackInterface => "Kind::CallbackInterface",
            _ => "Kind::Interface",
        };
        format!(
            "Interface {{ name: {name:?}, kind: {kind}, parent: {parent}, exposed: {}, secure_context: {iface_secure}, global: {}, no_interface_object: {}, constants: &[{}], attributes: &[{}], operations: &[{}], constructor: {constructor}, legacy_factories: &[{}], window_aliases: &[{}], special: Special {{ indexed_getter: {}, indexed_setter: {}, named_getter: {}, named_setter: {}, named_deleter: {}, index_count: {}, supported_names: {} }}, iterable: {iterable}, stringifier: {stringifier}, legacy_unenumerable_named_properties: {}, legacy_override_builtins: {} }}",
            iface.exposed,
            has_ext(&iface.ext, "Global"),
            has_ext(&iface.ext, "LegacyNoInterfaceObject"),
            constants.join(", "),
            attributes.join(", "),
            operations.join(", "),
            factories.join(", "),
            aliases.join(", "),
            special[0],
            special[1],
            special[2],
            special[3],
            special[4],
            special[5],
            special[6],
            has_ext(&iface.ext, "LegacyUnenumerableNamedProperties"),
            has_ext(&iface.ext, "LegacyOverrideBuiltIns"),
        )
    }
}

/// The generated Rust source for `model`.
pub fn emit(model: &Model) -> String {
    let mut e = Emitter {
        model,
        ops: Vec::new(),
        op_set: HashSet::new(),
    };
    let interfaces: Vec<String> = model
        .interfaces
        .iter()
        .map(|iface| e.interface(iface))
        .collect();
    let dictionaries: Vec<String> = model
        .dictionaries
        .iter()
        .map(|d| {
            let members: Vec<String> = d
                .members
                .iter()
                .map(|m| {
                    format!(
                        "DictMember {{ name: {:?}, ty: {}, required: {}, default: {} }}",
                        m.name,
                        e.ty(&m.ty),
                        m.required,
                        opt_default(&m.default)
                    )
                })
                .collect();
            let parent = match d.parent.as_deref().and_then(|p| model.dictionary(p)) {
                Some(p) => format!("Some({p})"),
                None => "None".into(),
            };
            format!(
                "Dictionary {{ name: {:?}, parent: {parent}, members: &[{}] }}",
                d.name,
                members.join(", ")
            )
        })
        .collect();
    let enums: Vec<String> = model
        .enums
        .iter()
        .map(|en| {
            let values: Vec<String> = en.values.iter().map(|v| format!("{v:?}")).collect();
            format!(
                "Enumeration {{ name: {:?}, values: &[{}] }}",
                en.name,
                values.join(", ")
            )
        })
        .collect();
    let callbacks: Vec<String> = model
        .callbacks
        .iter()
        .map(|c| {
            format!(
                "CallbackFunction {{ name: {:?}, ret: {}, args: {}, legacy_treat_non_object_as_null: {} }}",
                c.name,
                e.ty(&c.ret),
                e.args(&c.args),
                has_ext(&c.ext, "LegacyTreatNonObjectAsNull")
            )
        })
        .collect();

    let mut out = String::new();
    out.push_str("// Generated by crates/vidrio/bindgen from crates/vidrio/idl. Do not edit.\n\n");
    out.push_str("use super::meta::*;\n\n");
    out.push_str("/// Every member the implementation can be asked for.\n");
    out.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n#[allow(non_camel_case_types)]\npub enum Op {\n");
    for op in &e.ops {
        let _ = writeln!(out, "    {op},");
    }
    out.push_str("}\n\n");
    out.push_str("impl Op {\n    /// The variant's name (`Interface_get_attribute`, …).\n    pub fn name(self) -> &'static str {\n        match self {\n");
    for op in &e.ops {
        let _ = writeln!(out, "            Op::{op} => {op:?},");
    }
    out.push_str("        }\n    }\n}\n\n");
    let _ = writeln!(
        out,
        "pub static INTERFACES: &[Interface] = &[\n{}\n];\n",
        interfaces
            .iter()
            .map(|s| format!("    {s},"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let _ = writeln!(
        out,
        "pub static DICTIONARIES: &[Dictionary] = &[\n{}\n];\n",
        dictionaries
            .iter()
            .map(|s| format!("    {s},"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let _ = writeln!(
        out,
        "pub static ENUMS: &[Enumeration] = &[\n{}\n];\n",
        enums
            .iter()
            .map(|s| format!("    {s},"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let _ = writeln!(
        out,
        "pub static CALLBACKS: &[CallbackFunction] = &[\n{}\n];\n",
        callbacks
            .iter()
            .map(|s| format!("    {s},"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    out.push_str("/// The index of the interface, namespace or callback interface `name`.\n");
    out.push_str("pub fn interface_by_name(name: &str) -> Option<Id> {\n    match name {\n");
    for (i, iface) in model.interfaces.iter().enumerate() {
        let _ = writeln!(out, "        {:?} => Some({i}),", iface.name);
    }
    out.push_str("        _ => None,\n    }\n}\n\n");
    out.push_str("/// The index of the dictionary `name`.\n");
    out.push_str("pub fn dictionary_by_name(name: &str) -> Option<Id> {\n    match name {\n");
    for (i, d) in model.dictionaries.iter().enumerate() {
        let _ = writeln!(out, "        {:?} => Some({i}),", d.name);
    }
    out.push_str("        _ => None,\n    }\n}\n\n");
    out.push_str("/// Interface indices by name.\n#[allow(non_upper_case_globals, dead_code)]\npub mod ids {\n    use super::Id;\n");
    for (i, iface) in model.interfaces.iter().enumerate() {
        let _ = writeln!(out, "    pub const {}: Id = {i};", ident(&iface.name));
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit_source(source: &str) -> String {
        let defs = idl::parse(source).expect("parses");
        emit(&Model::build(defs).expect("model"))
    }

    #[test]
    fn emits_ops_tables_and_lookups() {
        let code = emit_source(
            r#"
            [Exposed=Window] interface Node : EventTarget {
              const unsigned short ELEMENT_NODE = 1;
              readonly attribute unsigned short nodeType;
              attribute DOMString? nodeValue;
              Node appendChild(Node node);
              undefined f(long a);
              undefined f(DOMString a, optional long b = 0);
            };
            [Exposed=Window] interface EventTarget { constructor(); };
            [Exposed=Window] interface NodeList { getter Node? item(unsigned long index); iterable<Node>; };
            interface mixin ParentNode { [Unscopable] undefined append((Node or DOMString)... nodes); };
            Node includes ParentNode;
            "#,
        );
        assert!(code.contains("    Node_get_nodeType,\n"));
        assert!(code.contains("Op::Node_get_nodeType => \"Node_get_nodeType\","));
        assert!(code.contains("    Node_set_nodeValue,\n"));
        assert!(code.contains("    ParentNode_append,\n"));
        assert!(code.contains("    NodeList_index_count,\n"));
        assert!(code.contains("    EventTarget_constructor,\n"));
        assert!(code.contains("name: \"f\", overloads: &[Overload { op: Op::Node_f, index: 0"));
        assert!(code.contains("length: 1, is_static: false"));
        assert!(code.contains("Ty::Nullable(&Ty::DomString)"));
        assert!(code.contains("Ty::Union(&[Ty::Interface(0), Ty::DomString])"));
        assert!(code.contains("parent: Some(1)"));
        assert!(code.contains("default: Some(DefaultValue::Number(0.0))"));
        assert!(code.contains("\"NodeList\" => Some(2),"));
        assert!(code.contains("pub const Node: Id = 0;"));
        assert!(code.contains("iterable: Iterable::Value(Ty::Interface(0))"));
        assert!(code.contains("unscopable: true"));
    }
}
