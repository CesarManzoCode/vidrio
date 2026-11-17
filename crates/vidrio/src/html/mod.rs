//! The HTML parser (HTML §13.2), fragment parsing (§13.4) and fragment
//! serialization (§13.3).
//!
//! [`Parser`] drives the tokenizer and the tree builder over an input
//! stream that can grow while parsing (network input, `document.write`).
//! It stops and hands back control at each `script` end tag so the caller
//! can prepare the script, and keeps the insertion point and script
//! nesting level that `document.write` needs (vault/CONTRACT.md §3.2: the
//! scripts run on the same thread, between calls to [`Parser::run`]).

pub mod foreign;
pub mod input;
pub mod serialize;
pub mod tokenizer;
pub mod tree_builder;

use std::cell::RefCell;
use std::rc::Rc;

use crate::dom::{Document, NodeId};
use input::InputStream;
use tokenizer::{State, Tokenizer};
use tree_builder::TreeBuilder;

pub use serialize::{html5lib_tree, serialize_fragment, serialize_outer};

/// Why [`Parser::run`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseStatus {
    /// Every available character was consumed; feed more or call
    /// [`Parser::end`].
    NeedInput,
    /// The tokenizer reached the insertion point (only from
    /// [`Parser::run_until_insertion_point`]).
    ReachedInsertionPoint,
    /// A `script` end tag was processed. The insertion point is now just
    /// before the next input character and the script nesting level was
    /// incremented: the caller prepares the script element, then calls
    /// [`Parser::script_finished`].
    Script(NodeId),
    /// The parser pause flag is set.
    Paused,
    /// Parsing stopped (the end-of-file token was processed).
    Done,
}

/// The outcome of pumping tokens through the tree builder.
enum Pump {
    NeedInput,
    Script(NodeId),
    Done,
}

/// Runs the tokenizer and tree builder until input runs out, a script end
/// tag is processed or parsing stops.
fn pump(
    tokenizer: &mut Tokenizer,
    builder: &mut TreeBuilder,
    input: &mut InputStream,
    d: &mut Document,
) -> Pump {
    loop {
        if builder.is_stopped() {
            return Pump::Done;
        }
        if !tokenizer.has_queued_tokens() {
            tokenizer.allow_cdata = builder.adjusted_current_is_foreign(d);
            input.compact();
        }
        let Some(token) = tokenizer.next_token(input) else {
            return Pump::NeedInput;
        };
        builder.process_token(d, token);
        if let Some(state) = builder.take_tokenizer_state() {
            tokenizer.set_state(state);
        }
        if let Some(script) = builder.take_script() {
            return Pump::Script(script);
        }
    }
}

/// An HTML parser for a document (HTML §13.2).
pub struct Parser {
    document: Rc<RefCell<Document>>,
    input: InputStream,
    tokenizer: Tokenizer,
    builder: TreeBuilder,
    script_nesting_level: u32,
    paused: bool,
    /// Insertion points saved at each script end tag ("old insertion
    /// point"), restored by [`Parser::script_finished`].
    saved_insertion_points: Vec<Option<usize>>,
}

impl Parser {
    /// A parser for `document`; `scripting` is whether scripting is enabled
    /// for it (it decides how `noscript` parses).
    pub fn new(document: Rc<RefCell<Document>>, scripting: bool) -> Parser {
        Parser {
            document,
            input: InputStream::new(),
            tokenizer: Tokenizer::new(),
            builder: TreeBuilder::new(scripting),
            script_nesting_level: 0,
            paused: false,
            saved_insertion_points: Vec::new(),
        }
    }

    pub fn document(&self) -> &Rc<RefCell<Document>> {
        &self.document
    }

    /// Appends decoded network input at the end of the input stream.
    pub fn feed(&mut self, text: &str) {
        self.input.push(text);
    }

    /// Marks the end of the input stream (or, for a script-created parser,
    /// inserts the explicit "EOF" of `document.close()`).
    pub fn end(&mut self) {
        self.input.close();
    }

    /// Runs the parser over the available input.
    pub fn run(&mut self) -> ParseStatus {
        if self.paused {
            return ParseStatus::Paused;
        }
        let document = Rc::clone(&self.document);
        let mut d = document.borrow_mut();
        match pump(
            &mut self.tokenizer,
            &mut self.builder,
            &mut self.input,
            &mut d,
        ) {
            Pump::Done => ParseStatus::Done,
            Pump::NeedInput => ParseStatus::NeedInput,
            Pump::Script(script) => {
                self.saved_insertion_points
                    .push(self.input.insertion_point());
                self.input.set_insertion_point_here();
                self.script_nesting_level += 1;
                ParseStatus::Script(script)
            }
        }
    }

    /// Runs the parser over the characters before the insertion point
    /// (the `document.write` step "process the characters that were
    /// inserted").
    pub fn run_until_insertion_point(&mut self) -> ParseStatus {
        self.input.set_stop_at_insertion_point(true);
        let status = self.run();
        self.input.set_stop_at_insertion_point(false);
        if status == ParseStatus::NeedInput && self.input.at_insertion_point() {
            return ParseStatus::ReachedInsertionPoint;
        }
        status
    }

    /// Ends the handling of a [`ParseStatus::Script`]: decrements the script
    /// nesting level (clearing the pause flag at zero) and restores the
    /// insertion point.
    pub fn script_finished(&mut self) {
        self.script_nesting_level = self.script_nesting_level.saturating_sub(1);
        if self.script_nesting_level == 0 {
            self.paused = false;
        }
        let saved = self.saved_insertion_points.pop().flatten();
        self.input.set_insertion_point(saved);
    }

    /// Inserts `text` at the insertion point (`document.write`).
    pub fn write(&mut self, text: &str) {
        self.input.insert(text);
    }

    /// True if the insertion point is defined.
    pub fn has_insertion_point(&self) -> bool {
        self.input.insertion_point().is_some()
    }

    /// Sets the insertion point just before the next input character (for
    /// executing pending parsing-blocking scripts).
    pub fn set_insertion_point_here(&mut self) {
        self.input.set_insertion_point_here();
    }

    /// Makes the insertion point undefined.
    pub fn clear_insertion_point(&mut self) {
        self.input.set_insertion_point(None);
    }

    pub fn script_nesting_level(&self) -> u32 {
        self.script_nesting_level
    }

    /// Increments the script nesting level (when the caller executes a
    /// pending parsing-blocking script itself).
    pub fn increment_script_nesting_level(&mut self) {
        self.script_nesting_level += 1;
    }

    /// Decrements the script nesting level, clearing the pause flag at 0.
    pub fn decrement_script_nesting_level(&mut self) {
        self.script_nesting_level = self.script_nesting_level.saturating_sub(1);
        if self.script_nesting_level == 0 {
            self.paused = false;
        }
    }

    /// Sets the parser pause flag (a pending parsing-blocking script).
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// True once parsing has stopped.
    pub fn is_done(&self) -> bool {
        self.builder.is_stopped()
    }

    /// The tree builder (insertion mode and open elements).
    pub fn tree_builder(&self) -> &TreeBuilder {
        &self.builder
    }
}

/// Parses `input` as a complete HTML document. Script end tags do not run
/// anything: their elements stay as parsed.
pub fn parse_document(input: &str, scripting: bool) -> Document {
    let mut document = Document::new_html();
    parse_into(&mut document, input, scripting);
    document
}

/// Parses `input` into `document`, which must be empty.
pub fn parse_into(document: &mut Document, input: &str, scripting: bool) {
    let mut stream = InputStream::new();
    stream.push(input);
    stream.close();
    let mut tokenizer = Tokenizer::new();
    let mut builder = TreeBuilder::new(scripting);
    loop {
        match pump(&mut tokenizer, &mut builder, &mut stream, document) {
            Pump::Script(_) => continue,
            Pump::Done | Pump::NeedInput => break,
        }
    }
}

/// The HTML fragment parsing algorithm (HTML §13.4) with `context` as the
/// context element, in `document` (the context element's node
/// document): returns a new `DocumentFragment` with the parsed nodes.
/// `scripting` is whether scripting is enabled for the context document.
/// Scripts in the fragment are marked already started.
pub fn parse_fragment(
    document: &mut Document,
    context: NodeId,
    input: &str,
    scripting: bool,
) -> NodeId {
    let fragment = document.create_document_fragment();
    // The parser's root `html` element stands for the temporary document's;
    // it stays outside every tree.
    let root = document.create_html_element("html");
    let mut builder = TreeBuilder::new_fragment(document, context, root, fragment, scripting);
    let mut tokenizer = Tokenizer::new();
    let state = match document.element(context) {
        Some(e) if e.name.ns == Some(crate::dom::Namespace::Html) => match e.name.local.as_str() {
            "title" | "textarea" => State::Rcdata,
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => State::Rawtext,
            "script" => State::ScriptData,
            "noscript" if scripting => State::Rawtext,
            "plaintext" => State::Plaintext,
            _ => State::Data,
        },
        _ => State::Data,
    };
    tokenizer.set_state(state);
    let mut stream = InputStream::new();
    stream.push(input);
    stream.close();
    loop {
        match pump(&mut tokenizer, &mut builder, &mut stream, document) {
            Pump::Script(_) => continue,
            Pump::Done | Pump::NeedInput => break,
        }
    }
    fragment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_pauses_at_scripts_and_takes_written_text() {
        let document = Rc::new(RefCell::new(Document::new_html()));
        let mut parser = Parser::new(Rc::clone(&document), true);
        parser.feed("<p>a<script>x</script>b");
        let ParseStatus::Script(script) = parser.run() else {
            panic!("script expected");
        };
        assert!(document.borrow().is_html_element(script, "script"));
        assert!(parser.has_insertion_point());
        assert_eq!(parser.script_nesting_level(), 1);
        // document.write from the script.
        parser.write("<i>w</i>");
        assert_eq!(
            parser.run_until_insertion_point(),
            ParseStatus::ReachedInsertionPoint
        );
        parser.script_finished();
        assert!(!parser.has_insertion_point());
        assert_eq!(parser.run(), ParseStatus::NeedInput);
        parser.end();
        assert_eq!(parser.run(), ParseStatus::Done);
        let d = document.borrow();
        let body = d.body().expect("body");
        assert_eq!(
            serialize_fragment(&d, body, true),
            "<p>a<script>x</script><i>w</i>b</p>"
        );
    }
}
