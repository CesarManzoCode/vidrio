//! Tree construction (HTML §13.2.6): every insertion mode, the adoption
//! agency algorithm, foster parenting, foreign content, `template`
//! contents, the form element pointer, the scripting flag and quirks mode
//! determination.
//!
//! Not implemented here (see LIMITATIONS.md): the sanitizer, declarative
//! shadow roots, content patching (`<template for>`), custom elements and
//! the speculative parser. A `<template shadowrootmode>` stays a plain
//! `template`.

use std::collections::HashSet;

use super::foreign;
use super::tokenizer::{DoctypeToken, State as TokenizerState, Tag, Token};
use crate::dom::{Atom, Document, Namespace, NodeId, QualName, QuirksMode};

/// An insertion mode (HTML §13.2.4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertionMode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InTemplate,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
    AfterAfterFrameset,
}

/// An entry of the list of active formatting elements, with the token
/// the element was created for.
#[derive(Clone, Debug)]
enum Formatting {
    Marker,
    Element(NodeId, Tag),
}

/// What processing a token asks for next.
enum Flow {
    Done,
    /// Process the token again with the rules of the current insertion
    /// mode.
    Reprocess(Token),
}

/// Pending text (HTML §13.2.4.5): characters waiting to become one Text
/// node at an insertion location.
#[derive(Clone, Debug)]
struct PendingText {
    parent: NodeId,
    reference: Option<NodeId>,
    data: String,
}

/// Where the adoption agency puts the new formatting element.
enum Bookmark {
    Replace,
    InsertAfter(NodeId),
}

type Location = (NodeId, Option<NodeId>);

fn is_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{C}' | '\r' | ' ')
}

/// The local name of `id` if it is an element in the HTML namespace.
fn html_name(d: &Document, id: NodeId) -> Option<&'static str> {
    let e = d.element(id)?;
    (e.name.ns == Some(Namespace::Html)).then(|| e.name.local.as_str())
}

fn is_html_one_of(d: &Document, id: NodeId, names: &[&str]) -> bool {
    html_name(d, id).is_some_and(|n| names.contains(&n))
}

fn ns_and_name(d: &Document, id: NodeId) -> Option<(Option<Namespace>, &'static str)> {
    let e = d.element(id)?;
    Some((e.name.ns, e.name.local.as_str()))
}

/// The special category (HTML §13.2.4.2).
fn is_special(d: &Document, id: NodeId) -> bool {
    match ns_and_name(d, id) {
        Some((Some(Namespace::Html), name)) => matches!(
            name,
            "address"
                | "applet"
                | "area"
                | "article"
                | "aside"
                | "base"
                | "basefont"
                | "bgsound"
                | "blockquote"
                | "body"
                | "br"
                | "button"
                | "caption"
                | "center"
                | "col"
                | "colgroup"
                | "dd"
                | "details"
                | "dir"
                | "div"
                | "dl"
                | "dt"
                | "embed"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "frame"
                | "frameset"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "head"
                | "header"
                | "hgroup"
                | "hr"
                | "html"
                | "iframe"
                | "img"
                | "input"
                | "keygen"
                | "li"
                | "link"
                | "listing"
                | "main"
                | "marquee"
                | "menu"
                | "meta"
                | "nav"
                | "noembed"
                | "noframes"
                | "noscript"
                | "object"
                | "ol"
                | "p"
                | "param"
                | "plaintext"
                | "pre"
                | "script"
                | "search"
                | "section"
                | "select"
                | "source"
                | "style"
                | "summary"
                | "table"
                | "tbody"
                | "td"
                | "template"
                | "textarea"
                | "tfoot"
                | "th"
                | "thead"
                | "title"
                | "tr"
                | "track"
                | "ul"
                | "wbr"
                | "xmp"
        ),
        Some((Some(Namespace::MathMl), name)) => {
            matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml")
        }
        Some((Some(Namespace::Svg), name)) => matches!(name, "foreignObject" | "desc" | "title"),
        _ => false,
    }
}

fn is_mathml_text_integration_point(d: &Document, id: NodeId) -> bool {
    matches!(
        ns_and_name(d, id),
        Some((Some(Namespace::MathMl), "mi" | "mo" | "mn" | "ms" | "mtext"))
    )
}

/// An HTML integration point (HTML §13.2.6). For `annotation-xml` the
/// element's attribute stands for its start tag's.
fn is_html_integration_point(d: &Document, id: NodeId) -> bool {
    match ns_and_name(d, id) {
        Some((Some(Namespace::MathMl), "annotation-xml")) => {
            d.get_attribute(id, "encoding").is_some_and(|e| {
                e.eq_ignore_ascii_case("text/html")
                    || e.eq_ignore_ascii_case("application/xhtml+xml")
            })
        }
        Some((Some(Namespace::Svg), "foreignObject" | "desc" | "title")) => true,
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Default,
    ListItem,
    Button,
    Table,
}

fn is_scope_boundary(d: &Document, id: NodeId, scope: Scope) -> bool {
    match ns_and_name(d, id) {
        Some((Some(Namespace::Html), name)) => match scope {
            Scope::Table => matches!(name, "html" | "table" | "template"),
            _ => {
                matches!(
                    name,
                    "applet"
                        | "caption"
                        | "html"
                        | "table"
                        | "td"
                        | "th"
                        | "marquee"
                        | "object"
                        | "select"
                        | "template"
                ) || (scope == Scope::ListItem && matches!(name, "ol" | "ul"))
                    || (scope == Scope::Button && name == "button")
            }
        },
        Some((Some(Namespace::MathMl), name)) => {
            scope != Scope::Table
                && matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml")
        }
        Some((Some(Namespace::Svg), name)) => {
            scope != Scope::Table && matches!(name, "foreignObject" | "desc" | "title")
        }
        _ => false,
    }
}

const QUIRKS_PUBLIC_EXACT: &[&str] = &[
    "-//W3O//DTD W3 HTML Strict 3.0//EN//",
    "-/W3C/DTD HTML 4.0 Transitional/EN",
    "HTML",
];

const QUIRKS_PUBLIC_PREFIXES: &[&str] = &[
    "+//Silmaril//dtd html Pro v0r11 19970101//",
    "-//AS//DTD HTML 3.0 asWedit + extensions//",
    "-//AdvaSoft Ltd//DTD HTML 3.0 asWedit + extensions//",
    "-//IETF//DTD HTML 2.0 Level 1//",
    "-//IETF//DTD HTML 2.0 Level 2//",
    "-//IETF//DTD HTML 2.0 Strict Level 1//",
    "-//IETF//DTD HTML 2.0 Strict Level 2//",
    "-//IETF//DTD HTML 2.0 Strict//",
    "-//IETF//DTD HTML 2.0//",
    "-//IETF//DTD HTML 2.1E//",
    "-//IETF//DTD HTML 3.0//",
    "-//IETF//DTD HTML 3.2 Final//",
    "-//IETF//DTD HTML 3.2//",
    "-//IETF//DTD HTML 3//",
    "-//IETF//DTD HTML Level 0//",
    "-//IETF//DTD HTML Level 1//",
    "-//IETF//DTD HTML Level 2//",
    "-//IETF//DTD HTML Level 3//",
    "-//IETF//DTD HTML Strict Level 0//",
    "-//IETF//DTD HTML Strict Level 1//",
    "-//IETF//DTD HTML Strict Level 2//",
    "-//IETF//DTD HTML Strict Level 3//",
    "-//IETF//DTD HTML Strict//",
    "-//IETF//DTD HTML//",
    "-//Metrius//DTD Metrius Presentational//",
    "-//Microsoft//DTD Internet Explorer 2.0 HTML Strict//",
    "-//Microsoft//DTD Internet Explorer 2.0 HTML//",
    "-//Microsoft//DTD Internet Explorer 2.0 Tables//",
    "-//Microsoft//DTD Internet Explorer 3.0 HTML Strict//",
    "-//Microsoft//DTD Internet Explorer 3.0 HTML//",
    "-//Microsoft//DTD Internet Explorer 3.0 Tables//",
    "-//Netscape Comm. Corp.//DTD HTML//",
    "-//Netscape Comm. Corp.//DTD Strict HTML//",
    "-//O'Reilly and Associates//DTD HTML 2.0//",
    "-//O'Reilly and Associates//DTD HTML Extended 1.0//",
    "-//O'Reilly and Associates//DTD HTML Extended Relaxed 1.0//",
    "-//SQ//DTD HTML 2.0 HoTMetaL + extensions//",
    "-//SoftQuad Software//DTD HoTMetaL PRO 6.0::19990601::extensions to HTML 4.0//",
    "-//SoftQuad//DTD HoTMetaL PRO 4.0::19971010::extensions to HTML 4.0//",
    "-//Spyglass//DTD HTML 2.0 Extended//",
    "-//Sun Microsystems Corp.//DTD HotJava HTML//",
    "-//Sun Microsystems Corp.//DTD HotJava Strict HTML//",
    "-//W3C//DTD HTML 3 1995-03-24//",
    "-//W3C//DTD HTML 3.2 Draft//",
    "-//W3C//DTD HTML 3.2 Final//",
    "-//W3C//DTD HTML 3.2//",
    "-//W3C//DTD HTML 3.2S Draft//",
    "-//W3C//DTD HTML 4.0 Frameset//",
    "-//W3C//DTD HTML 4.0 Transitional//",
    "-//W3C//DTD HTML Experimental 19960712//",
    "-//W3C//DTD HTML Experimental 970421//",
    "-//W3C//DTD W3 HTML//",
    "-//W3O//DTD W3 HTML 3.0//",
    "-//WebTechs//DTD Mozilla HTML 2.0//",
    "-//WebTechs//DTD Mozilla HTML//",
];

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.is_char_boundary(prefix.len())
        && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// The mode a DOCTYPE token sets (HTML §13.2.6.4.1).
pub fn doctype_mode(token: &DoctypeToken) -> QuirksMode {
    let public = token.public_id.as_deref();
    let system = token.system_id.as_deref();
    let public_starts = |prefix: &str| public.is_some_and(|p| starts_with_ignore_case(p, prefix));
    let system_missing_or_empty = system.is_none_or(str::is_empty);
    if token.force_quirks
        || token.name.as_deref() != Some("html")
        || public.is_some_and(|p| {
            QUIRKS_PUBLIC_EXACT
                .iter()
                .any(|&q| p.eq_ignore_ascii_case(q))
        })
        || system.is_some_and(|s| {
            s.eq_ignore_ascii_case("http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd")
        })
        || QUIRKS_PUBLIC_PREFIXES.iter().any(|&q| public_starts(q))
        || (system_missing_or_empty
            && (public_starts("-//W3C//DTD HTML 4.01 Frameset//")
                || public_starts("-//W3C//DTD HTML 4.01 Transitional//")))
    {
        return QuirksMode::Quirks;
    }
    if public_starts("-//W3C//DTD XHTML 1.0 Frameset//")
        || public_starts("-//W3C//DTD XHTML 1.0 Transitional//")
        || (!system_missing_or_empty
            && (public_starts("-//W3C//DTD HTML 4.01 Frameset//")
                || public_starts("-//W3C//DTD HTML 4.01 Transitional//")))
    {
        return QuirksMode::LimitedQuirks;
    }
    QuirksMode::NoQuirks
}

/// The tree construction stage.
#[derive(Clone, Debug)]
pub struct TreeBuilder {
    mode: InsertionMode,
    original_mode: InsertionMode,
    template_modes: Vec<InsertionMode>,
    open: Vec<NodeId>,
    active: Vec<Formatting>,
    head: Option<NodeId>,
    form: Option<NodeId>,
    frameset_ok: bool,
    /// Scripting is enabled for the parser (the inverse of "parser
    /// scripting disabled").
    scripting: bool,
    foster_parenting: bool,
    /// The fragment context element.
    context: Option<NodeId>,
    /// Fragment case: insertions into the root `html` element (first)
    /// go to the fragment (second) (the insertion target redirection map).
    redirect: Option<(NodeId, NodeId)>,
    pending_table_chars: String,
    pending_text: Option<PendingText>,
    /// Drop the next token if it is U+000A (after `pre`, `listing` and
    /// `textarea` start tags).
    ignore_lf: bool,
    tokenizer_state: Option<TokenizerState>,
    script: Option<NodeId>,
    svg_scripts: HashSet<NodeId>,
    stopped: bool,
}

impl TreeBuilder {
    /// A tree builder for a document parser.
    pub fn new(scripting: bool) -> TreeBuilder {
        TreeBuilder {
            mode: InsertionMode::Initial,
            original_mode: InsertionMode::Initial,
            template_modes: Vec::new(),
            open: Vec::new(),
            active: Vec::new(),
            head: None,
            form: None,
            frameset_ok: true,
            scripting,
            foster_parenting: false,
            context: None,
            redirect: None,
            pending_table_chars: String::new(),
            pending_text: None,
            ignore_lf: false,
            tokenizer_state: None,
            script: None,
            svg_scripts: HashSet::new(),
            stopped: false,
        }
    }

    /// A tree builder for the fragment parsing algorithm (HTML §13.4):
    /// `root` is the parser's `html` element, whose insertions go to
    /// `fragment`.
    pub fn new_fragment(
        d: &Document,
        context: NodeId,
        root: NodeId,
        fragment: NodeId,
        scripting: bool,
    ) -> TreeBuilder {
        let mut tb = TreeBuilder::new(scripting);
        tb.context = Some(context);
        tb.redirect = Some((root, fragment));
        tb.open.push(root);
        if d.is_html_element(context, "template") {
            tb.template_modes.push(InsertionMode::InTemplate);
        }
        tb.reset_insertion_mode(d);
        let mut node = Some(context);
        while let Some(n) = node {
            if d.is_html_element(n, "form") {
                tb.form = Some(n);
                break;
            }
            node = d.parent(n);
        }
        tb
    }

    pub fn mode(&self) -> InsertionMode {
        self.mode
    }

    /// True once parsing has stopped (after the end-of-file token).
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// The tokenizer state the last token asked for.
    pub fn take_tokenizer_state(&mut self) -> Option<TokenizerState> {
        self.tokenizer_state.take()
    }

    /// The `script` element whose end tag the last token was.
    pub fn take_script(&mut self) -> Option<NodeId> {
        self.script.take()
    }

    /// The stack of open elements, topmost first.
    pub fn open_elements(&self) -> &[NodeId] {
        &self.open
    }

    /// SVG `script` elements whose end tag the parser has seen (processed
    /// by SVG rules, not by HTML script preparation).
    pub fn svg_scripts(&self) -> &HashSet<NodeId> {
        &self.svg_scripts
    }

    fn current(&self) -> NodeId {
        *self.open.last().expect("stack of open elements is empty")
    }

    /// The adjusted current node (HTML §13.2.4.2).
    pub fn adjusted_current(&self) -> Option<NodeId> {
        if self.context.is_some() && self.open.len() == 1 {
            self.context
        } else {
            self.open.last().copied()
        }
    }

    /// The adjusted current node is an element outside the HTML namespace.
    pub fn adjusted_current_is_foreign(&self, d: &Document) -> bool {
        self.adjusted_current()
            .and_then(|n| d.element(n))
            .is_some_and(|e| e.name.ns != Some(Namespace::Html))
    }

    fn parsing_template_contents(&self, d: &Document) -> bool {
        self.open.iter().any(|&n| d.is_html_element(n, "template"))
            || self
                .context
                .is_some_and(|c| d.is_html_element(c, "template"))
    }

    fn has_template_on_stack(&self, d: &Document) -> bool {
        self.open.iter().any(|&n| d.is_html_element(n, "template"))
    }

    // ----- dispatch --------------------------------------------------------

    /// Processes one token from the tokenizer (the tree construction
    /// dispatcher).
    pub fn process_token(&mut self, d: &mut Document, token: Token) {
        if self.stopped {
            return;
        }
        if self.ignore_lf {
            self.ignore_lf = false;
            if token == Token::Char('\n') {
                return;
            }
        }
        if !matches!(token, Token::Char(_)) {
            self.flush_pending_text(d);
        }
        let mut flow = if self.use_html_rules(d, &token) {
            self.process_in_mode(d, self.mode, token)
        } else {
            self.foreign_content(d, token)
        };
        while let Flow::Reprocess(token) = flow {
            flow = self.process_in_mode(d, self.mode, token);
        }
    }

    fn use_html_rules(&self, d: &Document, token: &Token) -> bool {
        let Some(node) = self.adjusted_current() else {
            return true;
        };
        let Some((ns, name)) = ns_and_name(d, node) else {
            return true;
        };
        if ns == Some(Namespace::Html) {
            return true;
        }
        if is_mathml_text_integration_point(d, node) {
            match token {
                Token::StartTag(tag) if tag.name != "mglyph" && tag.name != "malignmark" => {
                    return true;
                }
                Token::Char(_) => return true,
                _ => {}
            }
        }
        if ns == Some(Namespace::MathMl)
            && name == "annotation-xml"
            && matches!(token, Token::StartTag(tag) if tag.name == "svg")
        {
            return true;
        }
        if is_html_integration_point(d, node)
            && matches!(token, Token::StartTag(_) | Token::Char(_))
        {
            return true;
        }
        matches!(token, Token::Eof)
    }

    fn process_in_mode(&mut self, d: &mut Document, mode: InsertionMode, token: Token) -> Flow {
        use InsertionMode::*;
        match mode {
            Initial => self.initial(d, token),
            BeforeHtml => self.before_html(d, token),
            BeforeHead => self.before_head(d, token),
            InHead => self.in_head(d, token),
            InHeadNoscript => self.in_head_noscript(d, token),
            AfterHead => self.after_head(d, token),
            InBody => self.in_body(d, token),
            Text => self.text(d, token),
            InTable => self.in_table(d, token),
            InTableText => self.in_table_text(d, token),
            InCaption => self.in_caption(d, token),
            InColumnGroup => self.in_column_group(d, token),
            InTableBody => self.in_table_body(d, token),
            InRow => self.in_row(d, token),
            InCell => self.in_cell(d, token),
            InTemplate => self.in_template(d, token),
            AfterBody => self.after_body(d, token),
            InFrameset => self.in_frameset(d, token),
            AfterFrameset => self.after_frameset(d, token),
            AfterAfterBody => self.after_after_body(d, token),
            AfterAfterFrameset => self.after_after_frameset(d, token),
        }
    }

    // ----- creating and inserting nodes (§13.2.6.1) -------------------------

    /// The appropriate place for inserting a node.
    fn appropriate_place(&self, d: &Document, override_target: Option<NodeId>) -> Location {
        let mut target = override_target.unwrap_or_else(|| self.current());
        let mut reference = None;
        if self.foster_parenting
            && is_html_one_of(d, target, &["table", "tbody", "tfoot", "thead", "tr"])
        {
            let last = self
                .open
                .iter()
                .rposition(|&n| d.is_html_element(n, "template") || d.is_html_element(n, "table"));
            match last {
                None => return (self.open[0], None),
                Some(i) if d.is_html_element(self.open[i], "template") => target = self.open[i],
                Some(i) => {
                    let table = self.open[i];
                    match d.parent(table) {
                        Some(parent) => {
                            target = parent;
                            reference = Some(table);
                        }
                        None => target = self.open[i.saturating_sub(1)],
                    }
                }
            }
        }
        if let Some(contents) = d.template_contents(target) {
            return (contents, None);
        }
        (target, reference)
    }

    /// The adjusted insertion location: the fragment parser's root `html`
    /// element redirects to the fragment.
    fn adjusted(&self, location: Location) -> Location {
        match self.redirect {
            Some((root, fragment)) if location.0 == root => (fragment, None),
            _ => location,
        }
    }

    /// "Create an element for the token" (without custom elements or the
    /// sanitizer), with parser form association.
    fn create_element_for_token(
        &mut self,
        d: &mut Document,
        tag: &Tag,
        ns: Namespace,
        intended_parent: NodeId,
    ) -> NodeId {
        let element = d.create_element(QualName::new(Some(ns), None, tag.name));
        for attr in &tag.attrs {
            d.append_attribute(element, attr.clone());
        }
        if let Some(form) = self.form {
            let form_associated = ns == Namespace::Html
                && matches!(
                    tag.name.as_str(),
                    "button"
                        | "fieldset"
                        | "input"
                        | "object"
                        | "output"
                        | "select"
                        | "textarea"
                        | "img"
                );
            let listed = tag.name != "img";
            if form_associated
                && !self.parsing_template_contents(d)
                && self.context.is_none()
                && (!listed || tag.attr("form").is_none())
                && d.root(intended_parent) == d.root(form)
            {
                if let Some(e) = d.element_mut(element) {
                    e.form_owner = Some(form);
                }
            }
        }
        element
    }

    /// "Insert an element at the adjusted insertion location".
    fn insert_element_at(&mut self, d: &mut Document, element: NodeId, location: Option<Location>) {
        let location = match location {
            Some(location) => location,
            None => self.appropriate_place(d, None),
        };
        let (parent, reference) = self.adjusted(location);
        if d.parent(element).is_some()
            || d.is_host_including_inclusive_ancestor(element, parent)
            || (parent == NodeId::DOCUMENT && d.document_element().is_some())
        {
            return;
        }
        if reference.is_some_and(|r| d.parent(r) != Some(parent)) {
            return;
        }
        d.insert_before(parent, element, reference);
    }

    /// "Insert a foreign element".
    fn insert_foreign_element(&mut self, d: &mut Document, tag: &Tag, ns: Namespace) -> NodeId {
        self.flush_pending_text(d);
        let (parent, _) = self.appropriate_place(d, None);
        let element = self.create_element_for_token(d, tag, ns, parent);
        self.insert_element_at(d, element, None);
        self.open.push(element);
        element
    }

    fn insert_html_element(&mut self, d: &mut Document, tag: &Tag) -> NodeId {
        self.insert_foreign_element(d, tag, Namespace::Html)
    }

    /// "Insert a character" for every character of `data`.
    fn insert_text(&mut self, d: &mut Document, data: &str) {
        if data.is_empty() {
            return;
        }
        let location = self.adjusted(self.appropriate_place(d, None));
        let moved = self
            .pending_text
            .as_ref()
            .is_some_and(|pending| (pending.parent, pending.reference) != location);
        if moved {
            self.flush_pending_text(d);
        }
        let (parent, reference) = location;
        if parent == NodeId::DOCUMENT {
            return;
        }
        if reference.is_some_and(|r| d.parent(r) != Some(parent)) {
            return;
        }
        match &mut self.pending_text {
            Some(pending) => pending.data.push_str(data),
            None => {
                self.pending_text = Some(PendingText {
                    parent,
                    reference,
                    data: data.to_string(),
                })
            }
        }
    }

    fn insert_char(&mut self, d: &mut Document, c: char) {
        let mut buf = [0u8; 4];
        self.insert_text(d, c.encode_utf8(&mut buf));
    }

    /// "Flush pending text".
    fn flush_pending_text(&mut self, d: &mut Document) {
        let Some(pending) = self.pending_text.take() else {
            return;
        };
        let previous = match pending.reference {
            Some(r) => d.prev_sibling(r),
            None => d.last_child(pending.parent),
        };
        match previous {
            Some(prev) if d.is_text(prev) => d.append_text(prev, &pending.data),
            _ => {
                let text = d.create_text(&pending.data);
                d.insert_before(pending.parent, text, pending.reference);
            }
        }
    }

    /// "Insert a comment", at `location` or the appropriate place.
    fn insert_comment(&mut self, d: &mut Document, data: &str, location: Option<Location>) {
        let location = match location {
            Some(location) => location,
            None => self.appropriate_place(d, None),
        };
        let (parent, reference) = self.adjusted(location);
        if reference.is_some_and(|r| d.parent(r) != Some(parent)) {
            return;
        }
        let comment = d.create_comment(data);
        d.insert_before(parent, comment, reference);
    }

    /// "Insert a processing instruction", at `location` or the appropriate
    /// place.
    fn insert_pi(
        &mut self,
        d: &mut Document,
        target: &str,
        data: &str,
        location: Option<Location>,
    ) {
        let location = match location {
            Some(location) => location,
            None => self.appropriate_place(d, None),
        };
        let (parent, reference) = self.adjusted(location);
        if reference.is_some_and(|r| d.parent(r) != Some(parent)) {
            return;
        }
        let pi = d.create_processing_instruction(target, data);
        d.insert_before(parent, pi, reference);
    }

    /// Inserts a comment or processing instruction token at `location`.
    fn insert_markup(&mut self, d: &mut Document, token: Token, location: Option<Location>) {
        match token {
            Token::Comment(data) => self.insert_comment(d, &data, location),
            Token::ProcessingInstruction { target, data } => {
                self.insert_pi(d, &target, &data, location)
            }
            _ => {}
        }
    }

    // ----- the stack of open elements ---------------------------------------

    fn pop(&mut self) -> Option<NodeId> {
        self.open.pop()
    }

    fn pop_until_html(&mut self, d: &Document, names: &[&str]) {
        while let Some(node) = self.pop() {
            if is_html_one_of(d, node, names) {
                break;
            }
        }
    }

    fn pop_until_node(&mut self, node: NodeId) {
        while let Some(n) = self.pop() {
            if n == node {
                break;
            }
        }
    }

    fn remove_from_stack(&mut self, node: NodeId) {
        if let Some(i) = self.open.iter().rposition(|&n| n == node) {
            self.open.remove(i);
        }
    }

    fn in_scope_by(&self, d: &Document, scope: Scope, target: impl Fn(NodeId) -> bool) -> bool {
        for &node in self.open.iter().rev() {
            if target(node) {
                return true;
            }
            if is_scope_boundary(d, node, scope) {
                return false;
            }
        }
        false
    }

    fn html_in_scope(&self, d: &Document, name: &str, scope: Scope) -> bool {
        self.in_scope_by(d, scope, |n| d.is_html_element(n, name))
    }

    fn any_html_in_scope(&self, d: &Document, names: &[&str], scope: Scope) -> bool {
        self.in_scope_by(d, scope, |n| is_html_one_of(d, n, names))
    }

    fn node_in_scope(&self, d: &Document, node: NodeId) -> bool {
        self.in_scope_by(d, Scope::Default, |n| n == node)
    }

    /// "Generate implied end tags", except for `except`.
    fn generate_implied_end_tags(&mut self, d: &Document, except: Option<&str>) {
        while let Some(&node) = self.open.last() {
            match html_name(d, node) {
                Some(name)
                    if Some(name) != except
                        && matches!(
                            name,
                            "dd" | "dt"
                                | "li"
                                | "optgroup"
                                | "option"
                                | "p"
                                | "rb"
                                | "rp"
                                | "rt"
                                | "rtc"
                        ) =>
                {
                    self.pop();
                }
                _ => break,
            }
        }
    }

    /// "Generate all implied end tags thoroughly".
    fn generate_all_implied_end_tags_thoroughly(&mut self, d: &Document) {
        while let Some(&node) = self.open.last() {
            if is_html_one_of(
                d,
                node,
                &[
                    "caption", "colgroup", "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp",
                    "rt", "rtc", "tbody", "td", "tfoot", "th", "thead", "tr",
                ],
            ) {
                self.pop();
            } else {
                break;
            }
        }
    }

    /// "Close a p element".
    fn close_p(&mut self, d: &Document) {
        self.generate_implied_end_tags(d, Some("p"));
        self.pop_until_html(d, &["p"]);
    }

    fn close_p_in_button_scope(&mut self, d: &Document) {
        if self.html_in_scope(d, "p", Scope::Button) {
            self.close_p(d);
        }
    }

    /// "Reset the insertion mode appropriately".
    fn reset_insertion_mode(&mut self, d: &Document) {
        use InsertionMode::*;
        for i in (0..self.open.len()).rev() {
            let last = i == 0;
            let node = match (last, self.context) {
                (true, Some(context)) => context,
                _ => self.open[i],
            };
            let mode = match html_name(d, node) {
                Some("td" | "th") if !last => Some(InCell),
                Some("tr") => Some(InRow),
                Some("tbody" | "thead" | "tfoot") => Some(InTableBody),
                Some("caption") => Some(InCaption),
                Some("colgroup") => Some(InColumnGroup),
                Some("table") => Some(InTable),
                Some("template") => Some(*self.template_modes.last().unwrap_or(&InTemplate)),
                Some("head") if !last => Some(InHead),
                Some("body") => Some(InBody),
                Some("frameset") => Some(InFrameset),
                Some("html") => Some(if self.head.is_none() {
                    BeforeHead
                } else {
                    AfterHead
                }),
                _ => None,
            };
            if let Some(mode) = mode {
                self.mode = mode;
                return;
            }
            if last {
                self.mode = InBody;
                return;
            }
        }
        self.mode = InBody;
    }

    /// Stops parsing (HTML §13.2.7, the parts that touch the tree).
    fn stop(&mut self, d: &mut Document) {
        self.flush_pending_text(d);
        self.open.clear();
        self.stopped = true;
    }

    // ----- the list of active formatting elements ---------------------------

    fn active_position(&self, node: NodeId) -> Option<usize> {
        self.active
            .iter()
            .position(|e| matches!(e, Formatting::Element(n, _) if *n == node))
    }

    fn push_marker(&mut self) {
        self.active.push(Formatting::Marker);
    }

    /// "Push onto the list of active formatting elements" (with the Noah's
    /// Ark clause).
    fn push_active(&mut self, element: NodeId, tag: Tag) {
        let mut same = 0;
        let mut earliest = None;
        for (i, entry) in self.active.iter().enumerate().rev() {
            match entry {
                Formatting::Marker => break,
                Formatting::Element(_, t) => {
                    if t.name == tag.name
                        && t.attrs.len() == tag.attrs.len()
                        && t.attrs.iter().all(|a| {
                            tag.attrs
                                .iter()
                                .any(|b| a.local == b.local && a.ns == b.ns && a.value == b.value)
                        })
                    {
                        same += 1;
                        earliest = Some(i);
                    }
                }
            }
        }
        if same >= 3 {
            if let Some(i) = earliest {
                self.active.remove(i);
            }
        }
        self.active.push(Formatting::Element(element, tag));
    }

    /// "Reconstruct the active formatting elements".
    fn reconstruct_active(&mut self, d: &mut Document) {
        let Some(last) = self.active.last() else {
            return;
        };
        match last {
            Formatting::Marker => return,
            Formatting::Element(n, _) if self.open.contains(n) => return,
            _ => {}
        }
        let mut i = self.active.len() - 1;
        while i > 0 {
            match &self.active[i - 1] {
                Formatting::Marker => break,
                Formatting::Element(n, _) if self.open.contains(n) => break,
                _ => i -= 1,
            }
        }
        for j in i..self.active.len() {
            let Formatting::Element(_, tag) = &self.active[j] else {
                continue;
            };
            let tag = tag.clone();
            let element = self.insert_html_element(d, &tag);
            self.active[j] = Formatting::Element(element, tag);
        }
    }

    /// "Clear the list of active formatting elements up to the last marker".
    fn clear_active_to_marker(&mut self) {
        while let Some(entry) = self.active.pop() {
            if matches!(entry, Formatting::Marker) {
                break;
            }
        }
    }

    /// The adoption agency algorithm (HTML §13.2.6.4.7).
    fn adoption_agency(&mut self, d: &mut Document, tag: &Tag) {
        let subject = tag.name.as_str();
        let current = self.current();
        if d.is_html_element(current, subject) && self.active_position(current).is_none() {
            self.pop();
            return;
        }
        for _ in 0..8 {
            // The last formatting element after the last marker named subject.
            let mut formatting = None;
            for entry in self.active.iter().rev() {
                match entry {
                    Formatting::Marker => break,
                    Formatting::Element(n, _) if d.is_html_element(*n, subject) => {
                        formatting = Some(*n);
                        break;
                    }
                    _ => {}
                }
            }
            let Some(formatting) = formatting else {
                self.any_other_end_tag(d, subject);
                return;
            };
            let Some(formatting_index) = self.open.iter().position(|&n| n == formatting) else {
                if let Some(i) = self.active_position(formatting) {
                    self.active.remove(i);
                }
                return;
            };
            if !self.node_in_scope(d, formatting) {
                return;
            }
            let furthest =
                (formatting_index + 1..self.open.len()).find(|&i| is_special(d, self.open[i]));
            let Some(furthest_index) = furthest else {
                self.open.truncate(formatting_index);
                if let Some(i) = self.active_position(formatting) {
                    self.active.remove(i);
                }
                return;
            };
            let furthest_block = self.open[furthest_index];
            let common_ancestor = self.open[formatting_index - 1];
            let mut bookmark = Bookmark::Replace;
            let mut node_index = furthest_index;
            let mut last_node = furthest_block;
            let mut inner = 0;
            loop {
                inner += 1;
                node_index -= 1;
                let node = self.open[node_index];
                if node == formatting {
                    break;
                }
                let mut position = self.active_position(node);
                if inner > 3 {
                    if let Some(p) = position {
                        self.active.remove(p);
                        position = None;
                    }
                }
                let Some(position) = position else {
                    self.open.remove(node_index);
                    continue;
                };
                let Formatting::Element(_, node_tag) = &self.active[position] else {
                    unreachable!("position of an element entry");
                };
                let node_tag = node_tag.clone();
                let new_element =
                    self.create_element_for_token(d, &node_tag, Namespace::Html, common_ancestor);
                self.active[position] = Formatting::Element(new_element, node_tag);
                self.open[node_index] = new_element;
                if last_node == furthest_block {
                    bookmark = Bookmark::InsertAfter(new_element);
                }
                d.append_child(new_element, last_node);
                last_node = new_element;
            }
            let (target, reference) =
                self.adjusted(self.appropriate_place(d, Some(common_ancestor)));
            if reference.is_none_or(|r| d.parent(r) == Some(target)) {
                d.remove(last_node);
                if !d.is_host_including_inclusive_ancestor(last_node, target)
                    && !(target == NodeId::DOCUMENT && d.document_element().is_some())
                {
                    d.insert_before(target, last_node, reference);
                }
            }
            let formatting_position = self
                .active_position(formatting)
                .expect("formatting element in list");
            let Formatting::Element(_, formatting_tag) = &self.active[formatting_position] else {
                unreachable!("position of an element entry");
            };
            let formatting_tag = formatting_tag.clone();
            let new_element =
                self.create_element_for_token(d, &formatting_tag, Namespace::Html, furthest_block);
            d.reparent_children(furthest_block, new_element);
            d.append_child(furthest_block, new_element);
            match bookmark {
                Bookmark::Replace => {
                    self.active[formatting_position] =
                        Formatting::Element(new_element, formatting_tag);
                }
                Bookmark::InsertAfter(after) => {
                    let p = self
                        .active_position(after)
                        .expect("bookmark element in list");
                    self.active
                        .insert(p + 1, Formatting::Element(new_element, formatting_tag));
                    if let Some(old) = self.active_position(formatting) {
                        self.active.remove(old);
                    }
                }
            }
            self.remove_from_stack(formatting);
            let furthest_position = self
                .open
                .iter()
                .position(|&n| n == furthest_block)
                .expect("furthest block on the stack");
            self.open.insert(furthest_position + 1, new_element);
        }
    }

    /// The "any other end tag" steps of "in body".
    fn any_other_end_tag(&mut self, d: &mut Document, name: &str) {
        for i in (0..self.open.len()).rev() {
            let node = self.open[i];
            if d.is_html_element(node, name) {
                self.generate_implied_end_tags(d, Some(name));
                self.pop_until_node(node);
                return;
            }
            if is_special(d, node) {
                return;
            }
        }
    }

    /// Generic raw text and RCDATA element parsing (HTML §13.2.6.2).
    fn parse_text_element(&mut self, d: &mut Document, tag: &Tag, state: TokenizerState) {
        self.insert_html_element(d, tag);
        self.tokenizer_state = Some(state);
        self.original_mode = self.mode;
        self.mode = InsertionMode::Text;
    }

    fn add_missing_attributes(&mut self, d: &mut Document, element: NodeId, tag: &Tag) {
        for attr in &tag.attrs {
            let present = d.element(element).is_some_and(|e| {
                e.attrs
                    .iter()
                    .any(|a| a.ns.is_none() && a.local == attr.local)
            });
            if !present {
                d.append_attribute(element, attr.clone());
            }
        }
    }

    // ----- the insertion modes (§13.2.6.4) ----------------------------------

    fn initial(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(c) if is_ws(c) => Flow::Done,
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, Some((NodeId::DOCUMENT, None)));
                Flow::Done
            }
            Token::Doctype(doctype) => {
                let node = d.create_document_type(
                    doctype.name.as_deref().unwrap_or(""),
                    doctype.public_id.as_deref().unwrap_or(""),
                    doctype.system_id.as_deref().unwrap_or(""),
                );
                if d.doctype().is_none() && d.document_element().is_none() {
                    d.append_child(NodeId::DOCUMENT, node);
                }
                if !d.is_srcdoc && !d.parser_cannot_change_mode {
                    d.mode = doctype_mode(&doctype);
                }
                self.mode = InsertionMode::BeforeHtml;
                Flow::Done
            }
            token => {
                if !d.is_srcdoc && !d.parser_cannot_change_mode {
                    d.mode = QuirksMode::Quirks;
                }
                self.mode = InsertionMode::BeforeHtml;
                Flow::Reprocess(token)
            }
        }
    }

    fn before_html(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Doctype(_) => Flow::Done,
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, Some((NodeId::DOCUMENT, None)));
                Flow::Done
            }
            Token::Char(c) if is_ws(c) => Flow::Done,
            Token::StartTag(tag) if tag.name == "html" => {
                let element =
                    self.create_element_for_token(d, &tag, Namespace::Html, NodeId::DOCUMENT);
                self.insert_element_at(d, element, Some((NodeId::DOCUMENT, None)));
                self.open.push(element);
                self.mode = InsertionMode::BeforeHead;
                Flow::Done
            }
            Token::EndTag(tag) if !matches!(tag.name.as_str(), "head" | "body" | "html" | "br") => {
                Flow::Done
            }
            token => {
                let element = d.create_html_element("html");
                self.insert_element_at(d, element, Some((NodeId::DOCUMENT, None)));
                self.open.push(element);
                self.mode = InsertionMode::BeforeHead;
                Flow::Reprocess(token)
            }
        }
    }

    fn before_head(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(c) if is_ws(c) => Flow::Done,
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, None);
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(tag) if tag.name == "html" => self.in_body(d, Token::StartTag(tag)),
            Token::StartTag(tag) if tag.name == "head" => {
                let head = self.insert_html_element(d, &tag);
                self.head = Some(head);
                self.mode = InsertionMode::InHead;
                Flow::Done
            }
            Token::EndTag(tag) if !matches!(tag.name.as_str(), "head" | "body" | "html" | "br") => {
                Flow::Done
            }
            token => {
                let head = self.insert_html_element(d, &Tag::new("head"));
                self.head = Some(head);
                self.mode = InsertionMode::InHead;
                Flow::Reprocess(token)
            }
        }
    }

    fn in_head(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(c) if is_ws(c) => {
                self.insert_char(d, c);
                Flow::Done
            }
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, None);
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(tag) => match tag.name.as_str() {
                "html" => self.in_body(d, Token::StartTag(tag)),
                "base" | "basefont" | "bgsound" | "link" | "meta" => {
                    self.insert_html_element(d, &tag);
                    self.pop();
                    Flow::Done
                }
                "title" => {
                    self.parse_text_element(d, &tag, TokenizerState::Rcdata);
                    Flow::Done
                }
                "noscript" if self.scripting => {
                    self.parse_text_element(d, &tag, TokenizerState::Rawtext);
                    Flow::Done
                }
                "noframes" | "style" => {
                    self.parse_text_element(d, &tag, TokenizerState::Rawtext);
                    Flow::Done
                }
                "noscript" => {
                    self.insert_html_element(d, &tag);
                    self.mode = InsertionMode::InHeadNoscript;
                    Flow::Done
                }
                "script" => {
                    self.flush_pending_text(d);
                    let (parent, _) = self.appropriate_place(d, None);
                    let element = self.create_element_for_token(d, &tag, Namespace::Html, parent);
                    let fragment = self.context.is_some();
                    if let Some(e) = d.element_mut(element) {
                        e.script.parser_inserted = true;
                        e.script.force_async = false;
                        // A fragment parser does not run scripts.
                        e.script.already_started = fragment;
                    }
                    self.insert_element_at(d, element, None);
                    self.open.push(element);
                    self.tokenizer_state = Some(TokenizerState::ScriptData);
                    self.original_mode = self.mode;
                    self.mode = InsertionMode::Text;
                    Flow::Done
                }
                "template" => {
                    self.push_marker();
                    self.frameset_ok = false;
                    self.mode = InsertionMode::InTemplate;
                    self.template_modes.push(InsertionMode::InTemplate);
                    self.flush_pending_text(d);
                    let (parent, _) = self.appropriate_place(d, None);
                    let template = self.create_element_for_token(d, &tag, Namespace::Html, parent);
                    let location = self.appropriate_place(d, None);
                    self.open.push(template);
                    // Declarative shadow roots and content patching are not
                    // supported: the template is inserted as a plain one.
                    self.insert_element_at(d, template, Some(location));
                    Flow::Done
                }
                "head" => Flow::Done,
                _ => {
                    self.pop();
                    self.mode = InsertionMode::AfterHead;
                    Flow::Reprocess(Token::StartTag(tag))
                }
            },
            Token::EndTag(tag) => match tag.name.as_str() {
                "head" => {
                    self.pop();
                    self.mode = InsertionMode::AfterHead;
                    Flow::Done
                }
                "body" | "html" | "br" => {
                    self.pop();
                    self.mode = InsertionMode::AfterHead;
                    Flow::Reprocess(Token::EndTag(tag))
                }
                "template" => {
                    if !self.has_template_on_stack(d) {
                        return Flow::Done;
                    }
                    self.generate_all_implied_end_tags_thoroughly(d);
                    self.pop_until_html(d, &["template"]);
                    self.clear_active_to_marker();
                    self.template_modes.pop();
                    self.reset_insertion_mode(d);
                    Flow::Done
                }
                _ => Flow::Done,
            },
            token => {
                self.pop();
                self.mode = InsertionMode::AfterHead;
                Flow::Reprocess(token)
            }
        }
    }

    fn in_head_noscript(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(d, token),
            Token::EndTag(ref tag) if tag.name == "noscript" => {
                self.pop();
                self.mode = InsertionMode::InHead;
                Flow::Done
            }
            Token::Char(c) if is_ws(c) => self.in_head(d, token),
            Token::Comment(_) | Token::ProcessingInstruction { .. } => self.in_head(d, token),
            Token::StartTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"
                ) =>
            {
                self.in_head(d, token)
            }
            Token::StartTag(ref tag) if matches!(tag.name.as_str(), "head" | "noscript") => {
                Flow::Done
            }
            Token::EndTag(ref tag) if tag.name != "br" => Flow::Done,
            token => {
                self.pop();
                self.mode = InsertionMode::InHead;
                Flow::Reprocess(token)
            }
        }
    }

    fn after_head(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(c) if is_ws(c) => {
                self.insert_char(d, c);
                Flow::Done
            }
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, None);
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(tag) => match tag.name.as_str() {
                "html" => self.in_body(d, Token::StartTag(tag)),
                "body" => {
                    self.insert_html_element(d, &tag);
                    self.frameset_ok = false;
                    self.mode = InsertionMode::InBody;
                    Flow::Done
                }
                "frameset" => {
                    self.insert_html_element(d, &tag);
                    self.mode = InsertionMode::InFrameset;
                    Flow::Done
                }
                "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script"
                | "style" | "template" | "title" => {
                    let Some(head) = self.head else {
                        return Flow::Done;
                    };
                    self.open.push(head);
                    let flow = self.in_head(d, Token::StartTag(tag));
                    self.remove_from_stack(head);
                    flow
                }
                "head" => Flow::Done,
                _ => {
                    self.insert_html_element(d, &Tag::new("body"));
                    self.frameset_ok = true;
                    self.mode = InsertionMode::InBody;
                    Flow::Reprocess(Token::StartTag(tag))
                }
            },
            Token::EndTag(tag) => match tag.name.as_str() {
                "template" => self.in_head(d, Token::EndTag(tag)),
                "body" | "html" | "br" => {
                    self.insert_html_element(d, &Tag::new("body"));
                    self.frameset_ok = true;
                    self.mode = InsertionMode::InBody;
                    Flow::Reprocess(Token::EndTag(tag))
                }
                _ => Flow::Done,
            },
            token => {
                self.insert_html_element(d, &Tag::new("body"));
                self.frameset_ok = true;
                self.mode = InsertionMode::InBody;
                Flow::Reprocess(token)
            }
        }
    }

    fn in_body(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char('\0') => Flow::Done,
            Token::Char(c) => {
                self.reconstruct_active(d);
                self.insert_char(d, c);
                if !is_ws(c) {
                    self.frameset_ok = false;
                }
                Flow::Done
            }
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, None);
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(tag) => self.in_body_start_tag(d, tag),
            Token::EndTag(tag) => self.in_body_end_tag(d, tag),
            Token::Eof => {
                if !self.template_modes.is_empty() {
                    return self.in_template(d, Token::Eof);
                }
                self.stop(d);
                Flow::Done
            }
        }
    }

    fn in_body_start_tag(&mut self, d: &mut Document, mut tag: Tag) -> Flow {
        match tag.name.as_str() {
            "html" => {
                if !self.has_template_on_stack(d) {
                    let html = self.open[0];
                    self.add_missing_attributes(d, html, &tag);
                }
            }
            "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script" | "style"
            | "template" | "title" => return self.in_head(d, Token::StartTag(tag)),
            "body" => {
                if self.open.len() == 1
                    || !d.is_html_element(self.open[1], "body")
                    || self.has_template_on_stack(d)
                {
                    return Flow::Done;
                }
                self.frameset_ok = false;
                let body = self.open[1];
                self.add_missing_attributes(d, body, &tag);
            }
            "frameset" => {
                if self.open.len() == 1
                    || !d.is_html_element(self.open[1], "body")
                    || !self.frameset_ok
                {
                    return Flow::Done;
                }
                let body = self.open[1];
                d.remove(body);
                self.open.truncate(1);
                self.insert_html_element(d, &tag);
                self.mode = InsertionMode::InFrameset;
            }
            "address" | "article" | "aside" | "blockquote" | "center" | "details" | "dialog"
            | "dir" | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "header"
            | "hgroup" | "main" | "menu" | "nav" | "ol" | "p" | "search" | "section"
            | "summary" | "ul" => {
                self.close_p_in_button_scope(d);
                self.insert_html_element(d, &tag);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_p_in_button_scope(d);
                if is_html_one_of(d, self.current(), &["h1", "h2", "h3", "h4", "h5", "h6"]) {
                    self.pop();
                }
                self.insert_html_element(d, &tag);
            }
            "pre" | "listing" => {
                self.close_p_in_button_scope(d);
                self.insert_html_element(d, &tag);
                self.ignore_lf = true;
                self.frameset_ok = false;
            }
            "form" => {
                let in_template = self.parsing_template_contents(d);
                if self.form.is_some() && !in_template {
                    return Flow::Done;
                }
                self.close_p_in_button_scope(d);
                let form = self.insert_html_element(d, &tag);
                if !in_template {
                    self.form = Some(form);
                }
            }
            "li" => {
                self.frameset_ok = false;
                for i in (0..self.open.len()).rev() {
                    let node = self.open[i];
                    if d.is_html_element(node, "li") {
                        self.generate_implied_end_tags(d, Some("li"));
                        self.pop_until_html(d, &["li"]);
                        break;
                    }
                    if is_special(d, node) && !is_html_one_of(d, node, &["address", "div", "p"]) {
                        break;
                    }
                }
                self.close_p_in_button_scope(d);
                self.insert_html_element(d, &tag);
            }
            "dd" | "dt" => {
                self.frameset_ok = false;
                for i in (0..self.open.len()).rev() {
                    let node = self.open[i];
                    if let Some(name @ ("dd" | "dt")) = html_name(d, node) {
                        self.generate_implied_end_tags(d, Some(name));
                        self.pop_until_html(d, &[name]);
                        break;
                    }
                    if is_special(d, node) && !is_html_one_of(d, node, &["address", "div", "p"]) {
                        break;
                    }
                }
                self.close_p_in_button_scope(d);
                self.insert_html_element(d, &tag);
            }
            "plaintext" => {
                self.close_p_in_button_scope(d);
                self.insert_html_element(d, &tag);
                self.tokenizer_state = Some(TokenizerState::Plaintext);
            }
            "button" => {
                if self.html_in_scope(d, "button", Scope::Default) {
                    self.generate_implied_end_tags(d, None);
                    self.pop_until_html(d, &["button"]);
                }
                self.reconstruct_active(d);
                self.insert_html_element(d, &tag);
                self.frameset_ok = false;
            }
            "a" => {
                let mut existing = None;
                for entry in self.active.iter().rev() {
                    match entry {
                        Formatting::Marker => break,
                        Formatting::Element(n, _) if d.is_html_element(*n, "a") => {
                            existing = Some(*n);
                            break;
                        }
                        _ => {}
                    }
                }
                if let Some(existing) = existing {
                    self.adoption_agency(d, &tag);
                    if let Some(i) = self.active_position(existing) {
                        self.active.remove(i);
                    }
                    self.remove_from_stack(existing);
                }
                self.reconstruct_active(d);
                let element = self.insert_html_element(d, &tag);
                self.push_active(element, tag);
            }
            "b" | "big" | "code" | "em" | "font" | "i" | "s" | "small" | "strike" | "strong"
            | "tt" | "u" => {
                self.reconstruct_active(d);
                let element = self.insert_html_element(d, &tag);
                self.push_active(element, tag);
            }
            "nobr" => {
                self.reconstruct_active(d);
                if self.html_in_scope(d, "nobr", Scope::Default) {
                    self.adoption_agency(d, &tag);
                    self.reconstruct_active(d);
                }
                let element = self.insert_html_element(d, &tag);
                self.push_active(element, tag);
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_active(d);
                self.insert_html_element(d, &tag);
                self.push_marker();
                self.frameset_ok = false;
            }
            "table" => {
                if d.mode != QuirksMode::Quirks {
                    self.close_p_in_button_scope(d);
                }
                self.insert_html_element(d, &tag);
                self.frameset_ok = false;
                self.mode = InsertionMode::InTable;
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" => {
                self.reconstruct_active(d);
                self.insert_html_element(d, &tag);
                self.pop();
                self.frameset_ok = false;
            }
            "input" => {
                if self.context.is_some_and(|c| d.is_html_element(c, "select")) {
                    return Flow::Done;
                }
                if self.html_in_scope(d, "select", Scope::Default) {
                    self.pop_until_html(d, &["select"]);
                }
                self.reconstruct_active(d);
                self.insert_html_element(d, &tag);
                self.pop();
                if !tag
                    .attr("type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("hidden"))
                {
                    self.frameset_ok = false;
                }
            }
            "param" | "source" | "track" => {
                self.insert_html_element(d, &tag);
                self.pop();
            }
            "hr" => {
                self.close_p_in_button_scope(d);
                if self.html_in_scope(d, "select", Scope::Default) {
                    self.generate_implied_end_tags(d, None);
                }
                self.insert_html_element(d, &tag);
                self.pop();
                self.frameset_ok = false;
            }
            "image" => {
                tag.name = Atom::new("img");
                return Flow::Reprocess(Token::StartTag(tag));
            }
            "textarea" => {
                self.insert_html_element(d, &tag);
                self.ignore_lf = true;
                self.tokenizer_state = Some(TokenizerState::Rcdata);
                self.original_mode = self.mode;
                self.frameset_ok = false;
                self.mode = InsertionMode::Text;
            }
            "xmp" => {
                self.close_p_in_button_scope(d);
                self.reconstruct_active(d);
                self.frameset_ok = false;
                self.parse_text_element(d, &tag, TokenizerState::Rawtext);
            }
            "iframe" => {
                self.frameset_ok = false;
                self.parse_text_element(d, &tag, TokenizerState::Rawtext);
            }
            "noembed" => self.parse_text_element(d, &tag, TokenizerState::Rawtext),
            "noscript" if self.scripting => {
                self.parse_text_element(d, &tag, TokenizerState::Rawtext)
            }
            "select" => {
                if self.context.is_some_and(|c| d.is_html_element(c, "select")) {
                    return Flow::Done;
                }
                if self.html_in_scope(d, "select", Scope::Default) {
                    self.pop_until_html(d, &["select"]);
                    return Flow::Done;
                }
                self.reconstruct_active(d);
                self.insert_html_element(d, &tag);
                self.frameset_ok = false;
            }
            "option" => {
                if self.html_in_scope(d, "select", Scope::Default) {
                    self.generate_implied_end_tags(d, Some("optgroup"));
                } else if d.is_html_element(self.current(), "option") {
                    self.pop();
                }
                self.reconstruct_active(d);
                self.insert_html_element(d, &tag);
            }
            "optgroup" => {
                if self.html_in_scope(d, "select", Scope::Default) {
                    self.generate_implied_end_tags(d, None);
                } else if d.is_html_element(self.current(), "option") {
                    self.pop();
                }
                self.reconstruct_active(d);
                self.insert_html_element(d, &tag);
            }
            "rb" | "rtc" => {
                if self.html_in_scope(d, "ruby", Scope::Default) {
                    self.generate_implied_end_tags(d, None);
                }
                self.insert_html_element(d, &tag);
            }
            "rp" | "rt" => {
                if self.html_in_scope(d, "ruby", Scope::Default) {
                    self.generate_implied_end_tags(d, Some("rtc"));
                }
                self.insert_html_element(d, &tag);
            }
            "math" => {
                self.reconstruct_active(d);
                foreign::adjust_mathml_attributes(&mut tag.attrs);
                foreign::adjust_foreign_attributes(&mut tag.attrs);
                self.insert_foreign_element(d, &tag, Namespace::MathMl);
                if tag.self_closing {
                    self.pop();
                }
            }
            "svg" => {
                self.reconstruct_active(d);
                foreign::adjust_svg_attributes(&mut tag.attrs);
                foreign::adjust_foreign_attributes(&mut tag.attrs);
                self.insert_foreign_element(d, &tag, Namespace::Svg);
                if tag.self_closing {
                    self.pop();
                }
            }
            "caption" | "col" | "colgroup" | "frame" | "head" | "tbody" | "td" | "tfoot" | "th"
            | "thead" | "tr" => {}
            _ => {
                self.reconstruct_active(d);
                self.insert_html_element(d, &tag);
            }
        }
        Flow::Done
    }

    fn in_body_end_tag(&mut self, d: &mut Document, tag: Tag) -> Flow {
        match tag.name.as_str() {
            "template" => return self.in_head(d, Token::EndTag(tag)),
            "body" => {
                if self.html_in_scope(d, "body", Scope::Default) {
                    self.mode = InsertionMode::AfterBody;
                }
            }
            "html" => {
                if self.html_in_scope(d, "body", Scope::Default) {
                    self.mode = InsertionMode::AfterBody;
                    return Flow::Reprocess(Token::EndTag(tag));
                }
            }
            "address" | "article" | "aside" | "blockquote" | "button" | "center" | "details"
            | "dialog" | "dir" | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer"
            | "header" | "hgroup" | "listing" | "main" | "menu" | "nav" | "ol" | "pre"
            | "search" | "section" | "select" | "summary" | "ul" => {
                let name = tag.name.as_str();
                if self.html_in_scope(d, name, Scope::Default) {
                    self.generate_implied_end_tags(d, None);
                    self.pop_until_html(d, &[name]);
                }
            }
            "form" => {
                if !self.parsing_template_contents(d) {
                    let node = self.form.take();
                    let Some(node) = node.filter(|&n| self.node_in_scope(d, n)) else {
                        return Flow::Done;
                    };
                    self.generate_implied_end_tags(d, None);
                    self.remove_from_stack(node);
                } else {
                    if !self.html_in_scope(d, "form", Scope::Default) {
                        return Flow::Done;
                    }
                    self.generate_implied_end_tags(d, None);
                    self.pop_until_html(d, &["form"]);
                }
            }
            "p" => {
                if !self.html_in_scope(d, "p", Scope::Button) {
                    self.insert_html_element(d, &Tag::new("p"));
                }
                self.close_p(d);
            }
            "li" => {
                if self.html_in_scope(d, "li", Scope::ListItem) {
                    self.generate_implied_end_tags(d, Some("li"));
                    self.pop_until_html(d, &["li"]);
                }
            }
            "dd" | "dt" => {
                let name = tag.name.as_str();
                if self.html_in_scope(d, name, Scope::Default) {
                    self.generate_implied_end_tags(d, Some(name));
                    self.pop_until_html(d, &[name]);
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
                if self.any_html_in_scope(d, HEADINGS, Scope::Default) {
                    self.generate_implied_end_tags(d, None);
                    self.pop_until_html(d, HEADINGS);
                }
            }
            "a" | "b" | "big" | "code" | "em" | "font" | "i" | "nobr" | "s" | "small"
            | "strike" | "strong" | "tt" | "u" => self.adoption_agency(d, &tag),
            "applet" | "marquee" | "object" => {
                let name = tag.name.as_str();
                if self.html_in_scope(d, name, Scope::Default) {
                    self.generate_implied_end_tags(d, None);
                    self.pop_until_html(d, &[name]);
                    self.clear_active_to_marker();
                }
            }
            "br" => return self.in_body_start_tag(d, Tag::new("br")),
            name => self.any_other_end_tag(d, name),
        }
        Flow::Done
    }

    fn text(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(c) => {
                self.insert_char(d, c);
                Flow::Done
            }
            Token::Eof => {
                let current = self.current();
                if d.is_html_element(current, "script") {
                    if let Some(e) = d.element_mut(current) {
                        e.script.already_started = true;
                    }
                }
                self.pop();
                self.mode = self.original_mode;
                Flow::Reprocess(Token::Eof)
            }
            Token::EndTag(tag) if tag.name == "script" => {
                let script = self.current();
                self.pop();
                self.mode = self.original_mode;
                self.script = Some(script);
                Flow::Done
            }
            _ => {
                self.pop();
                self.mode = self.original_mode;
                Flow::Done
            }
        }
    }

    fn in_table(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(_)
                if is_html_one_of(
                    d,
                    self.current(),
                    &["table", "tbody", "template", "tfoot", "thead", "tr"],
                ) =>
            {
                self.pending_table_chars.clear();
                self.original_mode = self.mode;
                self.mode = InsertionMode::InTableText;
                Flow::Reprocess(token)
            }
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, None);
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(tag) => match tag.name.as_str() {
                "caption" => {
                    self.clear_to_table_context(d);
                    self.push_marker();
                    self.insert_html_element(d, &tag);
                    self.mode = InsertionMode::InCaption;
                    Flow::Done
                }
                "colgroup" => {
                    self.clear_to_table_context(d);
                    self.insert_html_element(d, &tag);
                    self.mode = InsertionMode::InColumnGroup;
                    Flow::Done
                }
                "col" => {
                    self.clear_to_table_context(d);
                    self.insert_html_element(d, &Tag::new("colgroup"));
                    self.mode = InsertionMode::InColumnGroup;
                    Flow::Reprocess(Token::StartTag(tag))
                }
                "tbody" | "tfoot" | "thead" => {
                    self.clear_to_table_context(d);
                    self.insert_html_element(d, &tag);
                    self.mode = InsertionMode::InTableBody;
                    Flow::Done
                }
                "td" | "th" | "tr" => {
                    self.clear_to_table_context(d);
                    self.insert_html_element(d, &Tag::new("tbody"));
                    self.mode = InsertionMode::InTableBody;
                    Flow::Reprocess(Token::StartTag(tag))
                }
                "table" => {
                    if !self.html_in_scope(d, "table", Scope::Table) {
                        return Flow::Done;
                    }
                    self.pop_until_html(d, &["table"]);
                    self.reset_insertion_mode(d);
                    Flow::Reprocess(Token::StartTag(tag))
                }
                "style" | "script" | "template" => self.in_head(d, Token::StartTag(tag)),
                "input"
                    if tag
                        .attr("type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("hidden")) =>
                {
                    self.insert_html_element(d, &tag);
                    self.pop();
                    Flow::Done
                }
                "form" => {
                    let in_template = self.parsing_template_contents(d);
                    if self.form.is_some() && !in_template {
                        return Flow::Done;
                    }
                    let form = self.insert_html_element(d, &tag);
                    if !in_template {
                        self.form = Some(form);
                    }
                    self.pop();
                    Flow::Done
                }
                _ => self.in_table_anything_else(d, Token::StartTag(tag)),
            },
            Token::EndTag(tag) => match tag.name.as_str() {
                "table" => {
                    if self.html_in_scope(d, "table", Scope::Table) {
                        self.pop_until_html(d, &["table"]);
                        self.reset_insertion_mode(d);
                    }
                    Flow::Done
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot"
                | "th" | "thead" | "tr" => Flow::Done,
                "template" => self.in_head(d, Token::EndTag(tag)),
                _ => self.in_table_anything_else(d, Token::EndTag(tag)),
            },
            Token::Eof => self.in_body(d, Token::Eof),
            token => self.in_table_anything_else(d, token),
        }
    }

    fn in_table_anything_else(&mut self, d: &mut Document, token: Token) -> Flow {
        self.foster_parenting = true;
        let flow = self.in_body(d, token);
        self.foster_parenting = false;
        flow
    }

    fn clear_to_table_context(&mut self, d: &Document) {
        while !is_html_one_of(d, self.current(), &["table", "template", "html"]) {
            self.pop();
        }
    }

    fn clear_to_table_body_context(&mut self, d: &Document) {
        while !is_html_one_of(
            d,
            self.current(),
            &["tbody", "tfoot", "thead", "template", "html"],
        ) {
            self.pop();
        }
    }

    fn clear_to_table_row_context(&mut self, d: &Document) {
        while !is_html_one_of(d, self.current(), &["tr", "template", "html"]) {
            self.pop();
        }
    }

    fn in_table_text(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char('\0') => Flow::Done,
            Token::Char(c) => {
                self.pending_table_chars.push(c);
                Flow::Done
            }
            token => {
                let pending = std::mem::take(&mut self.pending_table_chars);
                if pending.chars().any(|c| !is_ws(c)) {
                    for c in pending.chars() {
                        self.in_table_anything_else(d, Token::Char(c));
                    }
                } else {
                    self.insert_text(d, &pending);
                }
                self.flush_pending_text(d);
                self.mode = self.original_mode;
                Flow::Reprocess(token)
            }
        }
    }

    fn close_caption(&mut self, d: &Document) -> bool {
        if !self.html_in_scope(d, "caption", Scope::Table) {
            return false;
        }
        self.generate_implied_end_tags(d, None);
        self.pop_until_html(d, &["caption"]);
        self.clear_active_to_marker();
        self.mode = InsertionMode::InTable;
        true
    }

    fn in_caption(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::EndTag(ref tag) if tag.name == "caption" => {
                self.close_caption(d);
                Flow::Done
            }
            Token::StartTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                if self.close_caption(d) {
                    Flow::Reprocess(token)
                } else {
                    Flow::Done
                }
            }
            Token::EndTag(ref tag) if tag.name == "table" => {
                if self.close_caption(d) {
                    Flow::Reprocess(token)
                } else {
                    Flow::Done
                }
            }
            Token::EndTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "body"
                        | "col"
                        | "colgroup"
                        | "html"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                Flow::Done
            }
            token => self.in_body(d, token),
        }
    }

    fn in_column_group(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(c) if is_ws(c) => {
                self.insert_char(d, c);
                Flow::Done
            }
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, None);
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(d, token),
            Token::StartTag(ref tag) if tag.name == "col" => {
                self.insert_html_element(d, tag);
                self.pop();
                Flow::Done
            }
            Token::EndTag(ref tag) if tag.name == "colgroup" => {
                if d.is_html_element(self.current(), "colgroup") {
                    self.pop();
                    self.mode = InsertionMode::InTable;
                }
                Flow::Done
            }
            Token::EndTag(ref tag) if tag.name == "col" => Flow::Done,
            Token::StartTag(ref tag) | Token::EndTag(ref tag) if tag.name == "template" => {
                self.in_head(d, token)
            }
            Token::Eof => self.in_body(d, token),
            token => {
                if !d.is_html_element(self.current(), "colgroup") {
                    return Flow::Done;
                }
                self.pop();
                self.mode = InsertionMode::InTable;
                Flow::Reprocess(token)
            }
        }
    }

    fn in_table_body(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::StartTag(ref tag) if tag.name == "tr" => {
                self.clear_to_table_body_context(d);
                self.insert_html_element(d, tag);
                self.mode = InsertionMode::InRow;
                Flow::Done
            }
            Token::StartTag(ref tag) if matches!(tag.name.as_str(), "th" | "td") => {
                self.clear_to_table_body_context(d);
                self.insert_html_element(d, &Tag::new("tr"));
                self.mode = InsertionMode::InRow;
                Flow::Reprocess(token)
            }
            Token::EndTag(ref tag) if matches!(tag.name.as_str(), "tbody" | "tfoot" | "thead") => {
                if self.html_in_scope(d, tag.name.as_str(), Scope::Table) {
                    self.clear_to_table_body_context(d);
                    self.pop();
                    self.mode = InsertionMode::InTable;
                }
                Flow::Done
            }
            Token::StartTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead"
                ) =>
            {
                self.leave_table_body(d, token)
            }
            Token::EndTag(ref tag) if tag.name == "table" => self.leave_table_body(d, token),
            Token::EndTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" | "tr"
                ) =>
            {
                Flow::Done
            }
            token => self.in_table(d, token),
        }
    }

    fn leave_table_body(&mut self, d: &mut Document, token: Token) -> Flow {
        if !self.any_html_in_scope(d, &["tbody", "thead", "tfoot"], Scope::Table) {
            return Flow::Done;
        }
        self.clear_to_table_body_context(d);
        self.pop();
        self.mode = InsertionMode::InTable;
        Flow::Reprocess(token)
    }

    fn leave_row(&mut self, d: &Document) -> bool {
        if !self.html_in_scope(d, "tr", Scope::Table) {
            return false;
        }
        self.clear_to_table_row_context(d);
        self.pop();
        self.mode = InsertionMode::InTableBody;
        true
    }

    fn in_row(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::StartTag(ref tag) if matches!(tag.name.as_str(), "th" | "td") => {
                self.clear_to_table_row_context(d);
                self.insert_html_element(d, tag);
                self.mode = InsertionMode::InCell;
                self.push_marker();
                Flow::Done
            }
            Token::EndTag(ref tag) if tag.name == "tr" => {
                self.leave_row(d);
                Flow::Done
            }
            Token::StartTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" | "tr"
                ) =>
            {
                if self.leave_row(d) {
                    Flow::Reprocess(token)
                } else {
                    Flow::Done
                }
            }
            Token::EndTag(ref tag) if tag.name == "table" => {
                if self.leave_row(d) {
                    Flow::Reprocess(token)
                } else {
                    Flow::Done
                }
            }
            Token::EndTag(ref tag) if matches!(tag.name.as_str(), "tbody" | "tfoot" | "thead") => {
                if !self.html_in_scope(d, tag.name.as_str(), Scope::Table) {
                    return Flow::Done;
                }
                if self.leave_row(d) {
                    Flow::Reprocess(token)
                } else {
                    Flow::Done
                }
            }
            Token::EndTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th"
                ) =>
            {
                Flow::Done
            }
            token => self.in_table(d, token),
        }
    }

    /// "Close the cell".
    fn close_cell(&mut self, d: &Document) {
        self.generate_implied_end_tags(d, None);
        self.pop_until_html(d, &["td", "th"]);
        self.clear_active_to_marker();
        self.mode = InsertionMode::InRow;
    }

    fn in_cell(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::EndTag(ref tag) if matches!(tag.name.as_str(), "td" | "th") => {
                let name = tag.name.as_str();
                if self.html_in_scope(d, name, Scope::Table) {
                    self.generate_implied_end_tags(d, None);
                    self.pop_until_html(d, &[name]);
                    self.clear_active_to_marker();
                    self.mode = InsertionMode::InRow;
                }
                Flow::Done
            }
            Token::StartTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                if !self.any_html_in_scope(d, &["td", "th"], Scope::Table) {
                    return Flow::Done;
                }
                self.close_cell(d);
                Flow::Reprocess(token)
            }
            Token::EndTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html"
                ) =>
            {
                Flow::Done
            }
            Token::EndTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "table" | "tbody" | "tfoot" | "thead" | "tr"
                ) =>
            {
                if !self.html_in_scope(d, tag.name.as_str(), Scope::Table) {
                    return Flow::Done;
                }
                self.close_cell(d);
                Flow::Reprocess(token)
            }
            token => self.in_body(d, token),
        }
    }

    fn in_template(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(_)
            | Token::Comment(_)
            | Token::ProcessingInstruction { .. }
            | Token::Doctype(_) => self.in_body(d, token),
            Token::StartTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "base"
                        | "basefont"
                        | "bgsound"
                        | "link"
                        | "meta"
                        | "noframes"
                        | "script"
                        | "style"
                        | "template"
                        | "title"
                ) =>
            {
                self.in_head(d, token)
            }
            Token::EndTag(ref tag) if tag.name == "template" => self.in_head(d, token),
            Token::StartTag(ref tag) => {
                let mode = match tag.name.as_str() {
                    "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => InsertionMode::InTable,
                    "col" => InsertionMode::InColumnGroup,
                    "tr" => InsertionMode::InTableBody,
                    "td" | "th" => InsertionMode::InRow,
                    _ => InsertionMode::InBody,
                };
                self.template_modes.pop();
                self.template_modes.push(mode);
                self.mode = mode;
                Flow::Reprocess(token)
            }
            Token::EndTag(_) => Flow::Done,
            Token::Eof => {
                if !self.has_template_on_stack(d) {
                    self.stop(d);
                    return Flow::Done;
                }
                self.pop_until_html(d, &["template"]);
                self.clear_active_to_marker();
                self.template_modes.pop();
                self.reset_insertion_mode(d);
                Flow::Reprocess(Token::Eof)
            }
        }
    }

    fn after_body(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(c) if is_ws(c) => self.in_body(d, token),
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                let html = self.open[0];
                self.insert_markup(d, token, Some((html, None)));
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(d, token),
            Token::EndTag(ref tag) if tag.name == "html" => {
                if self.context.is_none() {
                    self.mode = InsertionMode::AfterAfterBody;
                }
                Flow::Done
            }
            Token::Eof => {
                self.stop(d);
                Flow::Done
            }
            token => {
                self.mode = InsertionMode::InBody;
                Flow::Reprocess(token)
            }
        }
    }

    fn in_frameset(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(c) if is_ws(c) => {
                self.insert_char(d, c);
                Flow::Done
            }
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, None);
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(d, token),
            Token::StartTag(ref tag) if tag.name == "frameset" => {
                self.insert_html_element(d, tag);
                Flow::Done
            }
            Token::EndTag(ref tag) if tag.name == "frameset" => {
                if self.open.len() > 1 {
                    self.pop();
                    if self.context.is_none() && !d.is_html_element(self.current(), "frameset") {
                        self.mode = InsertionMode::AfterFrameset;
                    }
                }
                Flow::Done
            }
            Token::StartTag(ref tag) if tag.name == "frame" => {
                self.insert_html_element(d, tag);
                self.pop();
                Flow::Done
            }
            Token::StartTag(ref tag) if tag.name == "noframes" => self.in_head(d, token),
            Token::Eof => {
                self.stop(d);
                Flow::Done
            }
            _ => Flow::Done,
        }
    }

    fn after_frameset(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char(c) if is_ws(c) => {
                self.insert_char(d, c);
                Flow::Done
            }
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, None);
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(d, token),
            Token::EndTag(ref tag) if tag.name == "html" => {
                self.mode = InsertionMode::AfterAfterFrameset;
                Flow::Done
            }
            Token::StartTag(ref tag) if tag.name == "noframes" => self.in_head(d, token),
            Token::Eof => {
                self.stop(d);
                Flow::Done
            }
            _ => Flow::Done,
        }
    }

    fn after_after_body(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, Some((NodeId::DOCUMENT, None)));
                Flow::Done
            }
            Token::Doctype(_) => self.in_body(d, token),
            Token::Char(c) if is_ws(c) => self.in_body(d, token),
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(d, token),
            Token::Eof => {
                self.stop(d);
                Flow::Done
            }
            token => {
                self.mode = InsertionMode::InBody;
                Flow::Reprocess(token)
            }
        }
    }

    fn after_after_frameset(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, Some((NodeId::DOCUMENT, None)));
                Flow::Done
            }
            Token::Doctype(_) => self.in_body(d, token),
            Token::Char(c) if is_ws(c) => self.in_body(d, token),
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(d, token),
            Token::Eof => {
                self.stop(d);
                Flow::Done
            }
            Token::StartTag(ref tag) if tag.name == "noframes" => self.in_head(d, token),
            _ => Flow::Done,
        }
    }

    // ----- foreign content (§13.2.6.5) ----------------------------------------

    fn foreign_content(&mut self, d: &mut Document, token: Token) -> Flow {
        match token {
            Token::Char('\0') => {
                self.insert_char(d, '\u{FFFD}');
                Flow::Done
            }
            Token::Char(c) => {
                self.insert_char(d, c);
                if !is_ws(c) {
                    self.frameset_ok = false;
                }
                Flow::Done
            }
            Token::Comment(_) | Token::ProcessingInstruction { .. } => {
                self.insert_markup(d, token, None);
                Flow::Done
            }
            Token::Doctype(_) => Flow::Done,
            Token::StartTag(ref tag) if breaks_out_of_foreign_content(tag) => {
                self.leave_foreign(d, token)
            }
            Token::EndTag(ref tag) if matches!(tag.name.as_str(), "br" | "p") => {
                self.leave_foreign(d, token)
            }
            Token::StartTag(mut tag) => {
                let adjusted = self.adjusted_current().expect("adjusted current node");
                let ns = d
                    .element(adjusted)
                    .and_then(|e| e.name.ns)
                    .unwrap_or(Namespace::Html);
                if ns == Namespace::MathMl {
                    foreign::adjust_mathml_attributes(&mut tag.attrs);
                }
                if ns == Namespace::Svg {
                    tag.name = foreign::svg_tag_name(tag.name);
                    foreign::adjust_svg_attributes(&mut tag.attrs);
                }
                foreign::adjust_foreign_attributes(&mut tag.attrs);
                let element = self.insert_foreign_element(d, &tag, ns);
                if tag.self_closing {
                    if tag.name == "script" && ns == Namespace::Svg {
                        self.svg_script_end(element);
                    } else {
                        self.pop();
                    }
                }
                Flow::Done
            }
            Token::EndTag(ref tag)
                if tag.name == "script"
                    && matches!(
                        ns_and_name(d, self.current()),
                        Some((Some(Namespace::Svg), "script"))
                    ) =>
            {
                let script = self.current();
                self.svg_script_end(script);
                Flow::Done
            }
            Token::EndTag(tag) => {
                let mut i = self.open.len() - 1;
                loop {
                    if i == 0 {
                        return Flow::Done;
                    }
                    let node = self.open[i];
                    let matches = d.element(node).is_some_and(|e| {
                        e.name
                            .local
                            .as_str()
                            .eq_ignore_ascii_case(tag.name.as_str())
                    });
                    if matches {
                        self.open.truncate(i);
                        return Flow::Done;
                    }
                    i -= 1;
                    if d.element(self.open[i])
                        .is_some_and(|e| e.name.ns == Some(Namespace::Html))
                    {
                        return self.process_in_mode(d, self.mode, Token::EndTag(tag));
                    }
                }
            }
            Token::Eof => self.process_in_mode(d, self.mode, Token::Eof),
        }
    }

    /// The SVG `script` end tag steps: the element is popped and recorded
    /// for SVG script processing.
    fn svg_script_end(&mut self, script: NodeId) {
        self.pop();
        self.svg_scripts.insert(script);
    }

    /// The breakout steps of foreign content: pop to an HTML element or
    /// integration point and reprocess in the current insertion mode.
    fn leave_foreign(&mut self, d: &Document, token: Token) -> Flow {
        while let Some(&node) = self.open.last() {
            let html = d
                .element(node)
                .is_some_and(|e| e.name.ns == Some(Namespace::Html));
            if html
                || is_mathml_text_integration_point(d, node)
                || is_html_integration_point(d, node)
            {
                break;
            }
            self.pop();
        }
        Flow::Reprocess(token)
    }
}

/// Start tags that leave foreign content (HTML §13.2.6.5).
fn breaks_out_of_foreign_content(tag: &Tag) -> bool {
    match tag.name.as_str() {
        "b" | "big" | "blockquote" | "body" | "br" | "center" | "code" | "dd" | "div" | "dl"
        | "dt" | "em" | "embed" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "head" | "hr" | "i"
        | "img" | "li" | "listing" | "menu" | "meta" | "nobr" | "ol" | "p" | "pre" | "ruby"
        | "s" | "small" | "span" | "strong" | "strike" | "sub" | "sup" | "table" | "tt" | "u"
        | "ul" | "var" => true,
        "font" => tag
            .attrs
            .iter()
            .any(|a| matches!(a.local.as_str(), "color" | "face" | "size")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doctype(name: &str, public: Option<&str>, system: Option<&str>) -> DoctypeToken {
        DoctypeToken {
            name: Some(name.to_string()),
            public_id: public.map(str::to_string),
            system_id: system.map(str::to_string),
            force_quirks: false,
        }
    }

    #[test]
    fn doctype_modes() {
        assert_eq!(
            doctype_mode(&doctype("html", None, None)),
            QuirksMode::NoQuirks
        );
        assert_eq!(
            doctype_mode(&doctype("html", None, Some("about:legacy-compat"))),
            QuirksMode::NoQuirks
        );
        assert_eq!(
            doctype_mode(&doctype("htm", None, None)),
            QuirksMode::Quirks
        );
        assert_eq!(
            doctype_mode(&doctype(
                "html",
                Some("-//w3c//dtd html 4.01 transitional//en"),
                None
            )),
            QuirksMode::Quirks
        );
        assert_eq!(
            doctype_mode(&doctype(
                "html",
                Some("-//W3C//DTD HTML 4.01 Transitional//EN"),
                Some("http://www.w3.org/TR/html4/loose.dtd")
            )),
            QuirksMode::LimitedQuirks
        );
        assert_eq!(
            doctype_mode(&doctype(
                "html",
                Some("-//W3C//DTD XHTML 1.0 Strict//EN"),
                None
            )),
            QuirksMode::NoQuirks
        );
        assert_eq!(
            doctype_mode(&doctype("html", Some("html"), None)),
            QuirksMode::Quirks
        );
    }
}
