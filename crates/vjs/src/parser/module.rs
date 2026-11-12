//! Module items: `import` and `export` declarations with import attributes
//! (ECMA-262 §16.2).

use super::func::{FnSite, bound_names};
use super::stmt::Ctx;
use super::{Bind, Parser, R, Usage, string_value};
use crate::ast::{
    ExportDecl, ExportName, ExportSpecifier, ImportDecl, ImportSpecifier, ModuleRequest, Name,
    Stmt, StmtKind, Utf16,
};
use crate::lexer::T;

impl<'a> Parser<'a> {
    fn check_strict_string(&self) -> R<()> {
        if self.tok.legacy_octal {
            return Err(self.error(
                self.tok.span,
                "legacy octal escape sequences are not allowed in strict mode",
            ));
        }
        Ok(())
    }

    /// ModuleExportName: IdentifierName or a well-formed string literal.
    fn parse_module_export_name(&mut self) -> R<ExportName> {
        let span = self.tok.span;
        match self.tok.kind {
            T::String => {
                self.check_strict_string()?;
                let value = string_value(&self.tok);
                let Ok(name) = String::from_utf16(&value) else {
                    return Err(self.error(span, "module export names must be well-formed Unicode"));
                };
                self.next()?;
                Ok(ExportName {
                    span,
                    name,
                    is_string: true,
                })
            }
            T::Name => {
                let name = self.tok.name().to_string();
                self.next()?;
                Ok(ExportName {
                    span,
                    name,
                    is_string: false,
                })
            }
            _ => Err(self.unexpected()),
        }
    }

    /// FromClause string and its optional `with { key: "value", ... }`.
    fn parse_module_request(&mut self) -> R<ModuleRequest> {
        let start = self.start();
        if !self.is(T::String) {
            return Err(self.unexpected());
        }
        self.check_strict_string()?;
        let specifier = string_value(&self.tok);
        self.next()?;
        let mut attributes: Vec<(Utf16, Utf16)> = Vec::new();
        if self.eat_kw("with")? {
            self.expect(T::LBrace)?;
            let mut first = true;
            while !self.eat(T::RBrace)? {
                if !first {
                    self.expect(T::Comma)?;
                    if self.eat(T::RBrace)? {
                        break;
                    }
                }
                first = false;
                let key_span = self.tok.span;
                let key = match self.tok.kind {
                    T::String => {
                        self.check_strict_string()?;
                        string_value(&self.tok)
                    }
                    T::Name => self.tok.name().encode_utf16().collect(),
                    _ => return Err(self.unexpected()),
                };
                self.next()?;
                self.expect(T::Colon)?;
                if !self.is(T::String) {
                    return Err(self.unexpected());
                }
                self.check_strict_string()?;
                let value = string_value(&self.tok);
                self.next()?;
                if attributes.iter().any(|(k, _)| *k == key) {
                    return Err(self.error(key_span, "duplicate import attribute"));
                }
                attributes.push((key, value));
            }
        }
        Ok(ModuleRequest {
            span: self.span_from(start),
            specifier,
            attributes,
        })
    }

    fn declare_import(&mut self, name: &str, span: crate::ast::Span) -> R<()> {
        self.declare_name(name, Bind::Lexical, span)
    }

    /// ImportDeclaration; the current token is `import`.
    pub(crate) fn parse_import_decl(&mut self, start: u32) -> R<Stmt> {
        self.next()?;
        let mut specifiers = Vec::new();
        if !self.is(T::String) {
            if self.is(T::Name) {
                let (name, span) = self.parse_ident(Usage::Binding)?;
                self.declare_import(&name, span)?;
                specifiers.push(ImportSpecifier::Default(Name { span, name }));
                if self.eat(T::Comma)? && !self.is(T::Star) && !self.is(T::LBrace) {
                    return Err(self.unexpected());
                }
            }
            if self.eat(T::Star)? {
                self.expect_kw("as")?;
                let (name, span) = self.parse_ident(Usage::Binding)?;
                self.declare_import(&name, span)?;
                specifiers.push(ImportSpecifier::Namespace(Name { span, name }));
            } else if self.eat(T::LBrace)? {
                let mut first = true;
                while !self.eat(T::RBrace)? {
                    if !first {
                        self.expect(T::Comma)?;
                        if self.eat(T::RBrace)? {
                            break;
                        }
                    }
                    first = false;
                    let imported = self.parse_module_export_name()?;
                    let (local, span) = if self.eat_kw("as")? {
                        self.parse_ident(Usage::Binding)?
                    } else {
                        if imported.is_string {
                            return Err(
                                self.error(imported.span, "a string import name requires 'as'")
                            );
                        }
                        self.check_identifier(&imported.name, imported.span, Usage::Binding)?;
                        (imported.name.clone(), imported.span)
                    };
                    self.declare_import(&local, span)?;
                    specifiers.push(ImportSpecifier::Named {
                        imported,
                        local: Name { span, name: local },
                    });
                }
            } else if specifiers.is_empty() {
                return Err(self.unexpected());
            }
            self.expect_kw("from")?;
        }
        let source = self.parse_module_request()?;
        self.semicolon()?;
        Ok(Stmt {
            span: self.span_from(start),
            kind: StmtKind::Import(Box::new(ImportDecl {
                span: self.span_from(start),
                specifiers,
                source,
            })),
        })
    }

    /// ExportDeclaration; the current token is `export`.
    pub(crate) fn parse_export_decl(&mut self, start: u32) -> R<Stmt> {
        self.next()?;
        let decl = if self.eat(T::Star)? {
            let exported = if self.eat_kw("as")? {
                let name = self.parse_module_export_name()?;
                self.add_export_name(&name.name, name.span)?;
                Some(name)
            } else {
                None
            };
            self.expect_kw("from")?;
            let source = self.parse_module_request()?;
            self.semicolon()?;
            ExportDecl::All { exported, source }
        } else if self.is_kw("default") {
            let span = self.tok.span;
            self.next()?;
            self.add_export_name("default", span)?;
            let decl_start = self.start();
            let site = FnSite::Declaration {
                hanging: false,
                default_export: true,
            };
            if self.is_kw("function") {
                ExportDecl::DefaultFunction(Box::new(self.parse_function(decl_start, false, site)?))
            } else if self.is_async_function() {
                self.next()?;
                ExportDecl::DefaultFunction(Box::new(self.parse_function(decl_start, true, site)?))
            } else if self.is_kw("class") {
                ExportDecl::DefaultClass(Box::new(self.parse_class(decl_start, true, true)?))
            } else {
                let e = self.parse_assign(false)?;
                self.semicolon()?;
                ExportDecl::DefaultExpr(Box::new(e))
            }
        } else if self.eat(T::LBrace)? {
            let mut specifiers = Vec::new();
            let mut first = true;
            while !self.eat(T::RBrace)? {
                if !first {
                    self.expect(T::Comma)?;
                    if self.eat(T::RBrace)? {
                        break;
                    }
                }
                first = false;
                let spec_start = self.start();
                let local = self.parse_module_export_name()?;
                let exported = if self.eat_kw("as")? {
                    self.parse_module_export_name()?
                } else {
                    local.clone()
                };
                self.add_export_name(&exported.name, exported.span)?;
                specifiers.push(ExportSpecifier {
                    span: self.span_from(spec_start),
                    local,
                    exported,
                });
            }
            let source = if self.eat_kw("from")? {
                Some(self.parse_module_request()?)
            } else {
                None
            };
            if source.is_none() {
                for spec in &specifiers {
                    if spec.local.is_string {
                        return Err(self.error(
                            spec.local.span,
                            "a string cannot name a local binding; use 'from'",
                        ));
                    }
                    self.check_identifier(&spec.local.name, spec.local.span, Usage::Reference)?;
                    self.check_local_export(&spec.local.name, spec.local.span);
                }
            }
            self.semicolon()?;
            ExportDecl::Named { specifiers, source }
        } else if self.is_kw("var")
            || self.is_kw("const")
            || (self.is_kw("let") && self.is_let_decl(Ctx::List))
            || self.is_kw("function")
            || self.is_kw("class")
            || self.is_async_function()
        {
            let stmt = self.parse_statement(Ctx::List)?;
            let mut names = Vec::new();
            match &stmt.kind {
                StmtKind::Var(decl) => {
                    for declarator in &decl.declarations {
                        bound_names(&declarator.target, &mut names);
                    }
                }
                StmtKind::Function(f) => {
                    names.extend(f.name.iter().map(|n| (n.name.clone(), n.span)))
                }
                StmtKind::Class(c) => names.extend(c.name.iter().map(|n| (n.name.clone(), n.span))),
                _ => {}
            }
            for (name, span) in names {
                self.add_export_name(&name, span)?;
            }
            ExportDecl::Declaration(stmt)
        } else {
            return Err(self.unexpected());
        };
        Ok(Stmt {
            span: self.span_from(start),
            kind: StmtKind::Export(Box::new(decl)),
        })
    }
}
