//! Web IDL lexer and parser (Web IDL §2): the definitions the pinned
//! interface files use, as a syntax tree. Names are not resolved here
//! (see `model.rs`).

use std::fmt;

// ----- syntax tree -----------------------------------------------------------

/// An extended attribute (Web IDL §2.14).
#[derive(Clone, Debug, PartialEq)]
pub struct ExtAttr {
    pub name: String,
    pub value: ExtValue,
}

/// The right-hand side of an extended attribute.
#[derive(Clone, Debug, PartialEq)]
pub enum ExtValue {
    None,
    Ident(String),
    Str(String),
    Number(String),
    Wildcard,
    List(Vec<String>),
    Args(Vec<Argument>),
    NamedArgs(String, Vec<Argument>),
}

/// A type with its extended attributes.
#[derive(Clone, Debug, PartialEq)]
pub struct Type {
    pub kind: TypeKind,
    pub nullable: bool,
    pub ext: Vec<ExtAttr>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// A primitive type (normalized, for example `unsigned long long`) or
    /// the name of a definition.
    Named(String),
    Sequence(Box<Type>),
    FrozenArray(Box<Type>),
    ObservableArray(Box<Type>),
    Record(Box<Type>, Box<Type>),
    Promise(Box<Type>),
    Union(Vec<Type>),
}

impl Type {
    pub fn named(name: &str) -> Type {
        Type {
            kind: TypeKind::Named(name.to_string()),
            nullable: false,
            ext: Vec::new(),
        }
    }
}

/// A default value of an optional argument or dictionary member.
#[derive(Clone, Debug, PartialEq)]
pub enum DefaultValue {
    Null,
    Undefined,
    Bool(bool),
    Number(f64),
    Str(String),
    EmptySequence,
    EmptyDictionary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Argument {
    pub ext: Vec<ExtAttr>,
    pub name: String,
    pub ty: Type,
    pub optional: bool,
    pub variadic: bool,
    pub default: Option<DefaultValue>,
}

/// `getter`, `setter` or `deleter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Special {
    Getter,
    Setter,
    Deleter,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Member {
    Const {
        ext: Vec<ExtAttr>,
        ty: Type,
        name: String,
        value: DefaultValue,
    },
    Attribute {
        ext: Vec<ExtAttr>,
        name: String,
        ty: Type,
        readonly: bool,
        is_static: bool,
        inherit: bool,
        stringifier: bool,
    },
    Operation {
        ext: Vec<ExtAttr>,
        name: Option<String>,
        ret: Type,
        args: Vec<Argument>,
        is_static: bool,
        special: Option<Special>,
        stringifier: bool,
    },
    Constructor {
        ext: Vec<ExtAttr>,
        args: Vec<Argument>,
    },
    /// A bare `stringifier;`.
    Stringifier { ext: Vec<ExtAttr> },
    Iterable {
        ext: Vec<ExtAttr>,
        key: Option<Type>,
        value: Type,
        is_async: bool,
    },
    Maplike {
        ext: Vec<ExtAttr>,
        readonly: bool,
        key: Type,
        value: Type,
    },
    Setlike {
        ext: Vec<ExtAttr>,
        readonly: bool,
        value: Type,
    },
}

impl Member {
    pub fn ext(&self) -> &[ExtAttr] {
        match self {
            Member::Const { ext, .. }
            | Member::Attribute { ext, .. }
            | Member::Operation { ext, .. }
            | Member::Constructor { ext, .. }
            | Member::Stringifier { ext }
            | Member::Iterable { ext, .. }
            | Member::Maplike { ext, .. }
            | Member::Setlike { ext, .. } => ext,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DictMember {
    pub ext: Vec<ExtAttr>,
    pub name: String,
    pub ty: Type,
    pub required: bool,
    pub default: Option<DefaultValue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceKind {
    Interface,
    Mixin,
    CallbackInterface,
    Namespace,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Definition {
    Interface {
        ext: Vec<ExtAttr>,
        kind: InterfaceKind,
        name: String,
        parent: Option<String>,
        members: Vec<Member>,
        partial: bool,
    },
    Dictionary {
        ext: Vec<ExtAttr>,
        name: String,
        parent: Option<String>,
        members: Vec<DictMember>,
        partial: bool,
    },
    Enum {
        ext: Vec<ExtAttr>,
        name: String,
        values: Vec<String>,
    },
    Typedef {
        ext: Vec<ExtAttr>,
        ty: Type,
        name: String,
    },
    Callback {
        ext: Vec<ExtAttr>,
        name: String,
        ret: Type,
        args: Vec<Argument>,
    },
    Includes {
        target: String,
        mixin: String,
    },
}

/// Finds an extended attribute by name.
pub fn find_ext<'a>(ext: &'a [ExtAttr], name: &str) -> Option<&'a ExtAttr> {
    ext.iter().find(|a| a.name == name)
}

pub fn has_ext(ext: &[ExtAttr], name: &str) -> bool {
    find_ext(ext, name).is_some()
}

// ----- lexer -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Int(String),
    Float(String),
    Str(String),
    Punct(char),
    Ellipsis,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Ident(s) | Tok::Int(s) | Tok::Float(s) => write!(f, "{s}"),
            Tok::Str(s) => write!(f, "\"{s}\""),
            Tok::Punct(c) => write!(f, "{c}"),
            Tok::Ellipsis => write!(f, "..."),
        }
    }
}

/// A syntax error with the line it was found on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn lex(source: &str) -> Result<Vec<(Tok, usize)>, ParseError> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
        } else if c == '"' {
            let start = i + 1;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            if i >= chars.len() {
                return Err(ParseError {
                    line,
                    message: "unterminated string".to_string(),
                });
            }
            out.push((Tok::Str(chars[start..i].iter().collect()), line));
            i += 1;
        } else if c == '.' && chars.get(i + 1) == Some(&'.') && chars.get(i + 2) == Some(&'.') {
            out.push((Tok::Ellipsis, line));
            i += 3;
        } else if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let start = i;
            let mut float = false;
            if c == '0' && matches!(chars.get(i + 1), Some('x' | 'X')) {
                i += 2;
                while i < chars.len() && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
            } else {
                while i < chars.len() {
                    let d = chars[i];
                    if d.is_ascii_digit() {
                        i += 1;
                    } else if d == '.' {
                        float = true;
                        i += 1;
                    } else if d == 'e' || d == 'E' {
                        float = true;
                        i += 1;
                        if matches!(chars.get(i), Some('+' | '-')) {
                            i += 1;
                        }
                    } else {
                        break;
                    }
                }
            }
            let text: String = chars[start..i].iter().collect();
            out.push((
                if float {
                    Tok::Float(text)
                } else {
                    Tok::Int(text)
                },
                line,
            ));
        } else if is_ident_start(c) {
            let start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            out.push((Tok::Ident(chars[start..i].iter().collect()), line));
        } else {
            out.push((Tok::Punct(c), line));
            i += 1;
        }
    }
    Ok(out)
}

// ----- parser ----------------------------------------------------------------

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

type PResult<T> = Result<T, ParseError>;

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n).map(|t| &t.0)
    }

    fn line(&self) -> usize {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map_or(0, |t| t.1)
    }

    fn error<T>(&self, message: &str) -> PResult<T> {
        let found = self
            .peek()
            .map_or("end of input".to_string(), |t| t.to_string());
        Err(ParseError {
            line: self.line(),
            message: format!("{message} (found {found})"),
        })
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.0.clone());
        self.pos += 1;
        t
    }

    fn is_punct(&self, c: char) -> bool {
        self.peek() == Some(&Tok::Punct(c))
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.is_punct(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, c: char) -> PResult<()> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            self.error(&format!("expected `{c}`"))
        }
    }

    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s)) if s == w)
    }

    fn is_word_at(&self, n: usize, w: &str) -> bool {
        matches!(self.peek_at(n), Some(Tok::Ident(s)) if s == w)
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if self.is_word(w) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_word(&mut self, w: &str) -> PResult<()> {
        if self.eat_word(w) {
            Ok(())
        } else {
            self.error(&format!("expected `{w}`"))
        }
    }

    /// Any identifier, keywords included (argument and attribute names
    /// may be keywords).
    fn ident(&mut self) -> PResult<String> {
        match self.peek() {
            Some(Tok::Ident(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => self.error("expected an identifier"),
        }
    }

    // ----- extended attributes -----

    fn ext_attrs(&mut self) -> PResult<Vec<ExtAttr>> {
        let mut out = Vec::new();
        if !self.eat_punct('[') {
            return Ok(out);
        }
        loop {
            if self.eat_punct(']') {
                break;
            }
            let name = self.ident()?;
            let value = if self.eat_punct('=') {
                match self.next() {
                    Some(Tok::Ident(v)) => {
                        if self.is_punct('(') {
                            let args = self.arguments()?;
                            ExtValue::NamedArgs(v, args)
                        } else {
                            ExtValue::Ident(v)
                        }
                    }
                    Some(Tok::Str(v)) => ExtValue::Str(v),
                    Some(Tok::Int(v)) | Some(Tok::Float(v)) => ExtValue::Number(v),
                    Some(Tok::Punct('*')) => ExtValue::Wildcard,
                    Some(Tok::Punct('(')) => {
                        let mut list = Vec::new();
                        loop {
                            if self.eat_punct(')') {
                                break;
                            }
                            match self.next() {
                                Some(Tok::Ident(v)) | Some(Tok::Str(v)) | Some(Tok::Int(v)) => {
                                    list.push(v)
                                }
                                Some(Tok::Punct(',')) => {}
                                _ => return self.error("bad extended attribute list"),
                            }
                        }
                        ExtValue::List(list)
                    }
                    _ => return self.error("bad extended attribute value"),
                }
            } else if self.is_punct('(') {
                ExtValue::Args(self.arguments()?)
            } else {
                ExtValue::None
            };
            out.push(ExtAttr { name, value });
            if !self.eat_punct(',') {
                self.expect_punct(']')?;
                break;
            }
        }
        Ok(out)
    }

    // ----- types -----

    fn type_with_ext(&mut self) -> PResult<Type> {
        let ext = self.ext_attrs()?;
        let mut ty = self.type_()?;
        if !ext.is_empty() {
            let mut all = ext;
            all.append(&mut ty.ext);
            ty.ext = all;
        }
        Ok(ty)
    }

    fn type_(&mut self) -> PResult<Type> {
        if self.eat_punct('(') {
            let mut members = vec![self.type_with_ext()?];
            while self.eat_word("or") {
                members.push(self.type_with_ext()?);
            }
            self.expect_punct(')')?;
            let nullable = self.eat_punct('?');
            return Ok(Type {
                kind: TypeKind::Union(members),
                nullable,
                ext: Vec::new(),
            });
        }
        let kind = self.single_type()?;
        let nullable = self.eat_punct('?');
        Ok(Type {
            kind,
            nullable,
            ext: Vec::new(),
        })
    }

    fn generic_one(&mut self) -> PResult<Box<Type>> {
        self.expect_punct('<')?;
        let t = self.type_with_ext()?;
        self.expect_punct('>')?;
        Ok(Box::new(t))
    }

    fn single_type(&mut self) -> PResult<TypeKind> {
        let word = self.ident()?;
        let kind = match word.as_str() {
            "unsigned" => {
                let w = self.ident()?;
                match w.as_str() {
                    "short" => TypeKind::Named("unsigned short".into()),
                    "long" => {
                        if self.eat_word("long") {
                            TypeKind::Named("unsigned long long".into())
                        } else {
                            TypeKind::Named("unsigned long".into())
                        }
                    }
                    _ => return self.error("expected `short` or `long`"),
                }
            }
            "long" => {
                if self.eat_word("long") {
                    TypeKind::Named("long long".into())
                } else {
                    TypeKind::Named("long".into())
                }
            }
            "unrestricted" => {
                let w = self.ident()?;
                match w.as_str() {
                    "float" => TypeKind::Named("unrestricted float".into()),
                    "double" => TypeKind::Named("unrestricted double".into()),
                    _ => return self.error("expected `float` or `double`"),
                }
            }
            "sequence" => TypeKind::Sequence(self.generic_one()?),
            "FrozenArray" => TypeKind::FrozenArray(self.generic_one()?),
            "ObservableArray" => TypeKind::ObservableArray(self.generic_one()?),
            "Promise" => TypeKind::Promise(self.generic_one()?),
            "record" => {
                self.expect_punct('<')?;
                let k = self.type_with_ext()?;
                self.expect_punct(',')?;
                let v = self.type_with_ext()?;
                self.expect_punct('>')?;
                TypeKind::Record(Box::new(k), Box::new(v))
            }
            _ => TypeKind::Named(word),
        };
        Ok(kind)
    }

    // ----- values -----

    fn number(&mut self, negative: bool) -> PResult<f64> {
        let sign = if negative { -1.0 } else { 1.0 };
        match self.next() {
            Some(Tok::Int(s)) => Ok(sign * parse_int(&s)),
            Some(Tok::Float(s)) => match s.parse::<f64>() {
                Ok(v) => Ok(sign * v),
                Err(_) => self.error("bad number"),
            },
            Some(Tok::Ident(s)) if s == "Infinity" => Ok(sign * f64::INFINITY),
            _ => self.error("expected a number"),
        }
    }

    fn default_value(&mut self) -> PResult<DefaultValue> {
        if self.eat_punct('[') {
            self.expect_punct(']')?;
            return Ok(DefaultValue::EmptySequence);
        }
        if self.eat_punct('{') {
            self.expect_punct('}')?;
            return Ok(DefaultValue::EmptyDictionary);
        }
        if self.eat_punct('-') {
            return Ok(DefaultValue::Number(self.number(true)?));
        }
        match self.peek().cloned() {
            Some(Tok::Str(s)) => {
                self.pos += 1;
                Ok(DefaultValue::Str(s))
            }
            Some(Tok::Int(_)) | Some(Tok::Float(_)) => {
                Ok(DefaultValue::Number(self.number(false)?))
            }
            Some(Tok::Ident(w)) => {
                self.pos += 1;
                match w.as_str() {
                    "null" => Ok(DefaultValue::Null),
                    "undefined" => Ok(DefaultValue::Undefined),
                    "true" => Ok(DefaultValue::Bool(true)),
                    "false" => Ok(DefaultValue::Bool(false)),
                    "Infinity" => Ok(DefaultValue::Number(f64::INFINITY)),
                    "NaN" => Ok(DefaultValue::Number(f64::NAN)),
                    _ => self.error("bad default value"),
                }
            }
            _ => self.error("bad default value"),
        }
    }

    // ----- arguments -----

    fn arguments(&mut self) -> PResult<Vec<Argument>> {
        self.expect_punct('(')?;
        let mut out = Vec::new();
        if self.eat_punct(')') {
            return Ok(out);
        }
        loop {
            let ext = self.ext_attrs()?;
            let optional = self.eat_word("optional");
            let mut ty = self.type_with_ext()?;
            let ext = move_type_annotations(ext, &mut ty);
            let variadic = matches!(self.peek(), Some(Tok::Ellipsis));
            if variadic {
                self.pos += 1;
            }
            let name = self.ident()?;
            let default = if self.eat_punct('=') {
                Some(self.default_value()?)
            } else {
                None
            };
            out.push(Argument {
                ext,
                name: unescape(&name),
                ty,
                optional,
                variadic,
                default,
            });
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')')?;
        Ok(out)
    }

    // ----- members -----

    fn attribute_rest(
        &mut self,
        ext: Vec<ExtAttr>,
        is_static: bool,
        stringifier: bool,
    ) -> PResult<Member> {
        let inherit = self.eat_word("inherit");
        let readonly = self.eat_word("readonly");
        self.expect_word("attribute")?;
        let ty = self.type_with_ext()?;
        let name = self.ident()?;
        self.expect_punct(';')?;
        Ok(Member::Attribute {
            ext,
            name: unescape(&name),
            ty,
            readonly,
            is_static,
            inherit,
            stringifier,
        })
    }

    fn operation_rest(
        &mut self,
        ext: Vec<ExtAttr>,
        is_static: bool,
        special: Option<Special>,
        stringifier: bool,
    ) -> PResult<Member> {
        let ret = self.type_()?;
        let name = if self.is_punct('(') {
            None
        } else {
            Some(unescape(&self.ident()?))
        };
        let args = self.arguments()?;
        self.expect_punct(';')?;
        Ok(Member::Operation {
            ext,
            name,
            ret,
            args,
            is_static,
            special,
            stringifier,
        })
    }

    fn is_attribute_start(&self) -> bool {
        self.is_word("attribute")
            || (self.is_word("readonly") && self.is_word_at(1, "attribute"))
            || self.is_word("inherit")
    }

    fn member(&mut self) -> PResult<Member> {
        let ext = self.ext_attrs()?;
        if self.eat_word("const") {
            let ty = self.type_()?;
            let name = self.ident()?;
            self.expect_punct('=')?;
            let value = self.default_value()?;
            self.expect_punct(';')?;
            return Ok(Member::Const {
                ext,
                ty,
                name,
                value,
            });
        }
        if self.is_word("constructor") && self.peek_at(1) == Some(&Tok::Punct('(')) {
            self.pos += 1;
            let args = self.arguments()?;
            self.expect_punct(';')?;
            return Ok(Member::Constructor { ext, args });
        }
        if self.eat_word("stringifier") {
            if self.eat_punct(';') {
                return Ok(Member::Stringifier { ext });
            }
            if self.is_attribute_start() {
                return self.attribute_rest(ext, false, true);
            }
            return self.operation_rest(ext, false, None, true);
        }
        if self.eat_word("static") {
            if self.is_attribute_start() {
                return self.attribute_rest(ext, true, false);
            }
            return self.operation_rest(ext, true, None, false);
        }
        let readonly_collection = self.is_word("readonly")
            && (self.is_word_at(1, "maplike") || self.is_word_at(1, "setlike"));
        if readonly_collection {
            self.pos += 1;
        }
        if self.eat_word("maplike") {
            self.expect_punct('<')?;
            let key = self.type_with_ext()?;
            self.expect_punct(',')?;
            let value = self.type_with_ext()?;
            self.expect_punct('>')?;
            self.expect_punct(';')?;
            return Ok(Member::Maplike {
                ext,
                readonly: readonly_collection,
                key,
                value,
            });
        }
        if self.eat_word("setlike") {
            let value = self.generic_one()?;
            self.expect_punct(';')?;
            return Ok(Member::Setlike {
                ext,
                readonly: readonly_collection,
                value: *value,
            });
        }
        if self.is_attribute_start() {
            return self.attribute_rest(ext, false, false);
        }
        let is_async = if self.is_word("async") && self.is_word_at(1, "iterable") {
            self.pos += 1;
            true
        } else {
            self.eat_word("async_iterable")
        };
        if is_async || self.is_word("iterable") {
            if !is_async {
                self.pos += 1;
            } else if self.is_word("iterable") {
                self.pos += 1;
            }
            self.expect_punct('<')?;
            let first = self.type_with_ext()?;
            let (key, value) = if self.eat_punct(',') {
                (Some(first), self.type_with_ext()?)
            } else {
                (None, first)
            };
            self.expect_punct('>')?;
            if self.is_punct('(') {
                // Arguments of async iterables are parsed and dropped.
                self.arguments()?;
            }
            self.expect_punct(';')?;
            return Ok(Member::Iterable {
                ext,
                key,
                value,
                is_async,
            });
        }
        let special = if self.eat_word("getter") {
            Some(Special::Getter)
        } else if self.eat_word("setter") {
            Some(Special::Setter)
        } else if self.eat_word("deleter") {
            Some(Special::Deleter)
        } else {
            None
        };
        self.operation_rest(ext, false, special, false)
    }

    fn members(&mut self) -> PResult<Vec<Member>> {
        self.expect_punct('{')?;
        let mut out = Vec::new();
        while !self.eat_punct('}') {
            if self.peek().is_none() {
                return self.error("unterminated body");
            }
            out.push(self.member()?);
        }
        self.expect_punct(';')?;
        Ok(out)
    }

    fn dict_members(&mut self) -> PResult<Vec<DictMember>> {
        self.expect_punct('{')?;
        let mut out = Vec::new();
        while !self.eat_punct('}') {
            let ext = self.ext_attrs()?;
            let required = self.eat_word("required");
            let mut ty = self.type_with_ext()?;
            let ext = move_type_annotations(ext, &mut ty);
            let name = self.ident()?;
            let default = if self.eat_punct('=') {
                Some(self.default_value()?)
            } else {
                None
            };
            self.expect_punct(';')?;
            out.push(DictMember {
                ext,
                name: unescape(&name),
                ty,
                required,
                default,
            });
        }
        self.expect_punct(';')?;
        Ok(out)
    }

    fn parent(&mut self) -> PResult<Option<String>> {
        if self.eat_punct(':') {
            Ok(Some(self.ident()?))
        } else {
            Ok(None)
        }
    }

    // ----- definitions -----

    fn definition(&mut self) -> PResult<Definition> {
        let ext = self.ext_attrs()?;
        let partial = self.eat_word("partial");
        if self.eat_word("callback") {
            if self.eat_word("interface") {
                let name = self.ident()?;
                let members = self.members()?;
                return Ok(Definition::Interface {
                    ext,
                    kind: InterfaceKind::CallbackInterface,
                    name,
                    parent: None,
                    members,
                    partial,
                });
            }
            let name = self.ident()?;
            self.expect_punct('=')?;
            let ret = self.type_()?;
            let args = self.arguments()?;
            self.expect_punct(';')?;
            return Ok(Definition::Callback {
                ext,
                name,
                ret,
                args,
            });
        }
        if self.eat_word("interface") {
            let mixin = self.eat_word("mixin");
            let name = self.ident()?;
            let parent = if mixin { None } else { self.parent()? };
            let members = self.members()?;
            return Ok(Definition::Interface {
                ext,
                kind: if mixin {
                    InterfaceKind::Mixin
                } else {
                    InterfaceKind::Interface
                },
                name,
                parent,
                members,
                partial,
            });
        }
        if self.eat_word("namespace") {
            let name = self.ident()?;
            let members = self.members()?;
            return Ok(Definition::Interface {
                ext,
                kind: InterfaceKind::Namespace,
                name,
                parent: None,
                members,
                partial,
            });
        }
        if self.eat_word("dictionary") {
            let name = self.ident()?;
            let parent = self.parent()?;
            let members = self.dict_members()?;
            return Ok(Definition::Dictionary {
                ext,
                name,
                parent,
                members,
                partial,
            });
        }
        if self.eat_word("enum") {
            let name = self.ident()?;
            self.expect_punct('{')?;
            let mut values = Vec::new();
            loop {
                if self.eat_punct('}') {
                    break;
                }
                match self.next() {
                    Some(Tok::Str(s)) => values.push(s),
                    Some(Tok::Punct(',')) => {}
                    _ => return self.error("bad enum value"),
                }
            }
            self.expect_punct(';')?;
            return Ok(Definition::Enum { ext, name, values });
        }
        if self.eat_word("typedef") {
            let ty = self.type_with_ext()?;
            let name = self.ident()?;
            self.expect_punct(';')?;
            return Ok(Definition::Typedef { ext, ty, name });
        }
        if matches!(self.peek(), Some(Tok::Ident(_))) && self.is_word_at(1, "includes") {
            let target = self.ident()?;
            self.pos += 1;
            let mixin = self.ident()?;
            self.expect_punct(';')?;
            return Ok(Definition::Includes { target, mixin });
        }
        self.error("expected a definition")
    }
}

/// The extended attributes that annotate a type (Web IDL §2.13.29).
const TYPE_ANNOTATIONS: &[&str] = &[
    "AllowResizable",
    "AllowShared",
    "Clamp",
    "EnforceRange",
    "LegacyNullToEmptyString",
];

/// Moves the type annotations of an argument or dictionary member onto its
/// type and returns the remaining extended attributes.
fn move_type_annotations(ext: Vec<ExtAttr>, ty: &mut Type) -> Vec<ExtAttr> {
    let mut rest = Vec::new();
    for a in ext {
        if TYPE_ANNOTATIONS.contains(&a.name.as_str()) {
            ty.ext.push(a);
        } else {
            rest.push(a);
        }
    }
    rest
}

/// An integer literal: decimal, hexadecimal (`0x`) or octal (leading 0).
fn parse_int(s: &str) -> f64 {
    let (digits, radix) = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (h, 16)
    } else if s.len() > 1 && s.starts_with('0') {
        (&s[1..], 8)
    } else {
        (s, 10)
    };
    let mut v = 0f64;
    for c in digits.chars() {
        v = v * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0));
    }
    v
}

/// Removes the escaping underscore of an identifier (Web IDL §2.1).
fn unescape(name: &str) -> String {
    name.strip_prefix('_').unwrap_or(name).to_string()
}

/// Parses a whole IDL fragment.
pub fn parse(source: &str) -> Result<Vec<Definition>, ParseError> {
    let toks = lex(source)?;
    let mut p = Parser { toks, pos: 0 };
    let mut out = Vec::new();
    while p.peek().is_some() {
        out.push(p.definition()?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_interfaces_with_members_and_extended_attributes() {
        let defs = parse(
            r#"
            [Exposed=(Window,Worker), LegacyWindowAlias=webkitURL]
            interface URL : Base {
              constructor(USVString url, optional USVString base);
              static boolean canParse(USVString url, optional USVString base);
              stringifier attribute USVString href;
              readonly attribute (Event or undefined)? x;
              const unsigned short NONE = 0x10;
              getter Node? item(unsigned long index);
              iterable<USVString, USVString>;
              undefined _any(optional sequence<long> a = [], long... rest);
            };
            "#,
        )
        .expect("parses");
        let Definition::Interface {
            ext,
            name,
            parent,
            members,
            ..
        } = &defs[0]
        else {
            panic!("not an interface");
        };
        assert_eq!(name, "URL");
        assert_eq!(parent.as_deref(), Some("Base"));
        assert_eq!(
            ext[0].value,
            ExtValue::List(vec!["Window".into(), "Worker".into()])
        );
        assert_eq!(ext[1].value, ExtValue::Ident("webkitURL".into()));
        assert!(matches!(&members[0], Member::Constructor { args, .. } if args.len() == 2));
        assert!(
            matches!(&members[1], Member::Operation { is_static: true, name: Some(n), .. } if n == "canParse")
        );
        assert!(matches!(
            &members[2],
            Member::Attribute {
                stringifier: true,
                ..
            }
        ));
        match &members[3] {
            Member::Attribute { ty, readonly, .. } => {
                assert!(readonly);
                assert!(ty.nullable);
                assert!(matches!(&ty.kind, TypeKind::Union(m) if m.len() == 2));
            }
            _ => panic!(),
        }
        assert!(matches!(
            &members[4],
            Member::Const {
                value: DefaultValue::Number(n),
                ..
            } if *n == 16.0
        ));
        assert!(matches!(
            &members[5],
            Member::Operation {
                special: Some(Special::Getter),
                ..
            }
        ));
        assert!(matches!(&members[6], Member::Iterable { key: Some(_), .. }));
        match &members[7] {
            Member::Operation { name, args, .. } => {
                assert_eq!(name.as_deref(), Some("any"));
                assert_eq!(args[0].default, Some(DefaultValue::EmptySequence));
                assert!(args[1].variadic);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parses_other_definitions() {
        let defs = parse(
            r#"
            dictionary D : P { required long a; [EnforceRange] unsigned long b = 5; DOMString c = "x"; };
            enum E { "a", "b-c", };
            typedef (ArrayBuffer or [AllowShared] ArrayBufferView) BufferSource;
            callback Cb = undefined (any... args);
            callback interface Listener { undefined handleEvent(Event e); };
            Document includes ParentNode;
            namespace console { undefined log(any... data); };
            interface mixin M { readonly maplike<DOMString, long>; };
            [LegacyFactoryFunction=Image(optional unsigned long width)] interface I {};
            "#,
        )
        .expect("parses");
        assert_eq!(defs.len(), 9);
        match &defs[0] {
            Definition::Dictionary { members, .. } => {
                assert!(members[0].required);
                assert_eq!(members[1].ty.ext[0].name, "EnforceRange");
                assert_eq!(members[2].default, Some(DefaultValue::Str("x".into())));
            }
            _ => panic!(),
        }
        assert!(matches!(&defs[1], Definition::Enum { values, .. } if values[1] == "b-c"));
        assert!(
            matches!(&defs[5], Definition::Includes { target, mixin } if target == "Document" && mixin == "ParentNode")
        );
        assert!(matches!(
            &defs[7],
            Definition::Interface {
                kind: InterfaceKind::Mixin,
                ..
            }
        ));
        match &defs[8] {
            Definition::Interface { ext, .. } => {
                assert!(
                    matches!(&ext[0].value, ExtValue::NamedArgs(n, a) if n == "Image" && a.len() == 1)
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn negative_and_special_numbers() {
        let defs = parse("dictionary D { double a = -Infinity; double b = NaN; long c = -1; };")
            .expect("parses");
        match &defs[0] {
            Definition::Dictionary { members, .. } => {
                assert_eq!(
                    members[0].default,
                    Some(DefaultValue::Number(f64::NEG_INFINITY))
                );
                assert!(matches!(members[1].default, Some(DefaultValue::Number(n)) if n.is_nan()));
                assert_eq!(members[2].default, Some(DefaultValue::Number(-1.0)));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn reports_errors_with_lines() {
        let err = parse("interface A {\n  attribute ;\n};").unwrap_err();
        assert_eq!(err.line, 2);
    }
}
