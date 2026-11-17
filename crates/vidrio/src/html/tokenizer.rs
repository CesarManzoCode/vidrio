//! The HTML tokenizer (HTML §13.2.5): every state of the state machine,
//! including processing instructions and named character references.
//!
//! The tokenizer pulls characters from an [`InputStream`] one step at a
//! time and queues the tokens it emits. When the stream has no character
//! yet (and has not ended) a step waits without consuming anything, so
//! parsing resumes exactly where it stopped once more input arrives. Parse
//! errors are not reported.

use std::collections::VecDeque;

use super::input::{InputStream, Next};
use crate::dom::{Atom, Attribute};
use crate::generated::html_entities::ENTITIES;

/// A tokenizer state (HTML §13.2.5.1 to §13.2.5.84).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Data,
    Rcdata,
    Rawtext,
    ScriptData,
    Plaintext,
    TagOpen,
    EndTagOpen,
    TagName,
    RcdataLessThanSign,
    RcdataEndTagOpen,
    RcdataEndTagName,
    RawtextLessThanSign,
    RawtextEndTagOpen,
    RawtextEndTagName,
    ScriptDataLessThanSign,
    ScriptDataEndTagOpen,
    ScriptDataEndTagName,
    ScriptDataEscapeStart,
    ScriptDataEscapeStartDash,
    ScriptDataEscaped,
    ScriptDataEscapedDash,
    ScriptDataEscapedDashDash,
    ScriptDataEscapedLessThanSign,
    ScriptDataEscapedEndTagOpen,
    ScriptDataEscapedEndTagName,
    ScriptDataDoubleEscapeStart,
    ScriptDataDoubleEscaped,
    ScriptDataDoubleEscapedDash,
    ScriptDataDoubleEscapedDashDash,
    ScriptDataDoubleEscapedLessThanSign,
    ScriptDataDoubleEscapeEnd,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    AttributeValueDoubleQuoted,
    AttributeValueSingleQuoted,
    AttributeValueUnquoted,
    AfterAttributeValueQuoted,
    SelfClosingStartTag,
    BogusComment,
    MarkupDeclarationOpen,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentLessThanSign,
    CommentLessThanSignBang,
    CommentLessThanSignBangDash,
    CommentLessThanSignBangDashDash,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,
    Doctype,
    BeforeDoctypeName,
    DoctypeName,
    AfterDoctypeName,
    AfterDoctypePublicKeyword,
    BeforeDoctypePublicIdentifier,
    DoctypePublicIdentifierDoubleQuoted,
    DoctypePublicIdentifierSingleQuoted,
    AfterDoctypePublicIdentifier,
    BetweenDoctypePublicAndSystemIdentifiers,
    AfterDoctypeSystemKeyword,
    BeforeDoctypeSystemIdentifier,
    DoctypeSystemIdentifierDoubleQuoted,
    DoctypeSystemIdentifierSingleQuoted,
    AfterDoctypeSystemIdentifier,
    BogusDoctype,
    CdataSection,
    CdataSectionBracket,
    CdataSectionEnd,
    ProcessingInstructionOpen,
    ProcessingInstructionTarget,
    AfterProcessingInstructionTarget,
    ProcessingInstructionData,
    ProcessingInstructionQuestionable,
    CharacterReference,
    NamedCharacterReference,
    AmbiguousAmpersand,
    NumericCharacterReference,
    HexadecimalCharacterReferenceStart,
    HexadecimalCharacterReference,
    DecimalCharacterReference,
    NumericCharacterReferenceEnd,
}

/// A DOCTYPE token; `None` is "missing".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DoctypeToken {
    pub name: Option<String>,
    pub public_id: Option<String>,
    pub system_id: Option<String>,
    pub force_quirks: bool,
}

/// A start or end tag token. Attribute names are lowercase local names
/// without namespace; the tree builder adjusts foreign ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub name: Atom,
    pub attrs: Vec<Attribute>,
    pub self_closing: bool,
}

impl Tag {
    /// A tag named `name` with no attributes.
    pub fn new(name: &str) -> Tag {
        Tag {
            name: Atom::new(name),
            attrs: Vec::new(),
            self_closing: false,
        }
    }

    /// The value of the attribute named `name`.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.local == *name)
            .map(|a| a.value.as_str())
    }
}

/// A token of HTML §13.2.5.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Doctype(DoctypeToken),
    StartTag(Tag),
    EndTag(Tag),
    Comment(String),
    ProcessingInstruction { target: String, data: String },
    Char(char),
    Eof,
}

fn is_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{C}' | ' ')
}

/// Code points that replace C1 controls in numeric character references
/// (HTML §13.2.5.84).
fn c1_replacement(code: u32) -> Option<u32> {
    Some(match code {
        0x80 => 0x20AC,
        0x82 => 0x201A,
        0x83 => 0x0192,
        0x84 => 0x201E,
        0x85 => 0x2026,
        0x86 => 0x2020,
        0x87 => 0x2021,
        0x88 => 0x02C6,
        0x89 => 0x2030,
        0x8A => 0x0160,
        0x8B => 0x2039,
        0x8C => 0x0152,
        0x8E => 0x017D,
        0x91 => 0x2018,
        0x92 => 0x2019,
        0x93 => 0x201C,
        0x94 => 0x201D,
        0x95 => 0x2022,
        0x96 => 0x2013,
        0x97 => 0x2014,
        0x98 => 0x02DC,
        0x99 => 0x2122,
        0x9A => 0x0161,
        0x9B => 0x203A,
        0x9C => 0x0153,
        0x9E => 0x017E,
        0x9F => 0x0178,
        _ => return None,
    })
}

/// The longest named character reference that is a prefix of the next
/// input characters: `Ok(Some((length, index into ENTITIES)))`, `Ok(None)`
/// if there is none, `Err(())` if more input is needed to decide.
fn match_named_reference(input: &InputStream) -> Result<Option<(usize, usize)>, ()> {
    let (mut lo, mut hi) = (0, ENTITIES.len());
    let mut best = None;
    let mut k = 0;
    loop {
        let c = match input.peek(k) {
            Next::Char(c) => c,
            Next::Eof => break,
            Next::Wait => return Err(()),
        };
        if !(c.is_ascii_alphanumeric() || c == ';') {
            break;
        }
        let b = c as u8;
        let slice = &ENTITIES[lo..hi];
        let start = lo + slice.partition_point(|e| e.0.len() <= k || e.0.as_bytes()[k] < b);
        let end = lo + slice.partition_point(|e| e.0.len() <= k || e.0.as_bytes()[k] <= b);
        if start == end {
            break;
        }
        lo = start;
        hi = end;
        k += 1;
        if ENTITIES[lo].0.len() == k {
            best = Some((k, lo));
        }
        if c == ';' {
            break;
        }
    }
    Ok(best)
}

/// The HTML tokenizer.
#[derive(Clone, Debug)]
pub struct Tokenizer {
    state: State,
    return_state: State,
    queue: VecDeque<Token>,
    tag_is_end: bool,
    tag_name: String,
    tag_self_closing: bool,
    attrs: Vec<(String, String)>,
    comment: String,
    doctype: DoctypeToken,
    pi_target: String,
    pi_data: String,
    temp: String,
    char_ref_code: u32,
    last_start_tag: Option<Atom>,
    /// The adjusted current node is an element outside the HTML namespace
    /// (lets `<![CDATA[` open a CDATA section). Set by the tree builder.
    pub allow_cdata: bool,
    done: bool,
}

impl Default for Tokenizer {
    fn default() -> Tokenizer {
        Tokenizer::new()
    }
}

impl Tokenizer {
    pub fn new() -> Tokenizer {
        Tokenizer {
            state: State::Data,
            return_state: State::Data,
            queue: VecDeque::new(),
            tag_is_end: false,
            tag_name: String::new(),
            tag_self_closing: false,
            attrs: Vec::new(),
            comment: String::new(),
            doctype: DoctypeToken::default(),
            pi_target: String::new(),
            pi_data: String::new(),
            temp: String::new(),
            char_ref_code: 0,
            last_start_tag: None,
            allow_cdata: false,
            done: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Switches the state machine (the tree builder does this for RCDATA,
    /// RAWTEXT, script data and PLAINTEXT content).
    pub fn set_state(&mut self, state: State) {
        self.state = state;
    }

    /// True once the end-of-file token has been emitted.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The next token, or `None` if the input has no more characters yet.
    pub fn next_token(&mut self, input: &mut InputStream) -> Option<Token> {
        loop {
            if let Some(token) = self.queue.pop_front() {
                return Some(token);
            }
            if self.done || !self.step(input) {
                return None;
            }
        }
    }

    /// True if tokens are queued from the last step.
    pub fn has_queued_tokens(&self) -> bool {
        !self.queue.is_empty()
    }

    // ----- emitting --------------------------------------------------------

    fn emit_char(&mut self, c: char) {
        self.queue.push_back(Token::Char(c));
    }

    fn emit_str(&mut self, s: &str) {
        for c in s.chars() {
            self.emit_char(c);
        }
    }

    fn emit_eof(&mut self) {
        self.queue.push_back(Token::Eof);
        self.done = true;
    }

    fn new_tag(&mut self, end: bool) {
        self.tag_is_end = end;
        self.tag_name.clear();
        self.tag_self_closing = false;
        self.attrs.clear();
    }

    fn new_attr(&mut self, name: &str) {
        self.attrs.push((name.to_string(), String::new()));
    }

    fn attr_name_push(&mut self, c: char) {
        if let Some(attr) = self.attrs.last_mut() {
            attr.0.push(c);
        }
    }

    fn attr_value_push(&mut self, c: char) {
        if let Some(attr) = self.attrs.last_mut() {
            attr.1.push(c);
        }
    }

    fn emit_tag(&mut self) {
        let name = Atom::new(&self.tag_name);
        let mut attrs: Vec<Attribute> = Vec::with_capacity(self.attrs.len());
        for (attr_name, value) in self.attrs.drain(..) {
            // A later attribute with the same name is dropped (§13.2.5.33).
            if attrs.iter().any(|a| a.local == *attr_name.as_str()) {
                continue;
            }
            attrs.push(Attribute::new(&attr_name, &value));
        }
        let tag = Tag {
            name,
            attrs,
            self_closing: self.tag_self_closing,
        };
        if self.tag_is_end {
            self.queue.push_back(Token::EndTag(tag));
        } else {
            self.last_start_tag = Some(name);
            self.queue.push_back(Token::StartTag(tag));
        }
    }

    fn emit_comment(&mut self) {
        let data = std::mem::take(&mut self.comment);
        self.queue.push_back(Token::Comment(data));
    }

    fn emit_doctype(&mut self) {
        let doctype = std::mem::take(&mut self.doctype);
        self.queue.push_back(Token::Doctype(doctype));
    }

    fn emit_pi(&mut self) {
        let target = std::mem::take(&mut self.pi_target);
        let data = std::mem::take(&mut self.pi_data);
        self.queue
            .push_back(Token::ProcessingInstruction { target, data });
    }

    fn new_doctype(&mut self) {
        self.doctype = DoctypeToken::default();
    }

    fn doctype_quirks_emit(&mut self) {
        self.doctype.force_quirks = true;
        self.emit_doctype();
    }

    fn doctype_eof(&mut self) {
        self.doctype.force_quirks = true;
        self.emit_doctype();
        self.emit_eof();
    }

    fn is_appropriate_end_tag(&self) -> bool {
        self.last_start_tag
            .is_some_and(|name| name == *self.tag_name.as_str())
    }

    fn in_attribute(&self) -> bool {
        matches!(
            self.return_state,
            State::AttributeValueDoubleQuoted
                | State::AttributeValueSingleQuoted
                | State::AttributeValueUnquoted
        )
    }

    /// "Flush code points consumed as a character reference".
    fn flush_char_ref(&mut self) {
        let temp = std::mem::take(&mut self.temp);
        if self.in_attribute() {
            if let Some(attr) = self.attrs.last_mut() {
                attr.1.push_str(&temp);
            }
        } else {
            self.emit_str(&temp);
        }
    }

    /// "Convert the temporary buffer to a comment".
    fn temp_to_comment(&mut self) {
        self.comment = format!("?{}", self.temp);
    }

    fn reconsume(&mut self, input: &mut InputStream, c: Option<char>, state: State) {
        if c.is_some() {
            input.unconsume();
        }
        self.state = state;
    }

    /// The shared "end tag name" states of RCDATA, RAWTEXT and script data
    /// (HTML §13.2.5.11, .14, .17 and .25); `text` is the state to go back
    /// to when the tag is not an appropriate end tag.
    fn text_end_tag_name(&mut self, input: &mut InputStream, c: Option<char>, text: State) {
        match c {
            Some(c) if is_ws(c) && self.is_appropriate_end_tag() => {
                self.state = State::BeforeAttributeName;
            }
            Some('/') if self.is_appropriate_end_tag() => self.state = State::SelfClosingStartTag,
            Some('>') if self.is_appropriate_end_tag() => {
                self.state = State::Data;
                self.emit_tag();
            }
            Some(c) if c.is_ascii_alphabetic() => {
                self.tag_name.push(c.to_ascii_lowercase());
                self.temp.push(c);
            }
            _ => {
                self.emit_str("</");
                let temp = std::mem::take(&mut self.temp);
                self.emit_str(&temp);
                self.reconsume(input, c, text);
            }
        }
    }

    // ----- the state machine -----------------------------------------------

    /// Runs one step. Returns false when it must wait for more input.
    fn step(&mut self, input: &mut InputStream) -> bool {
        use State::*;
        match self.state {
            MarkupDeclarationOpen => return self.markup_declaration_open(input),
            NamedCharacterReference => return self.named_character_reference(input),
            AfterDoctypeName => return self.after_doctype_name(input),
            NumericCharacterReferenceEnd => {
                self.numeric_character_reference_end();
                return true;
            }
            _ => {}
        }
        let c = match input.next() {
            Next::Char(c) => Some(c),
            Next::Eof => None,
            Next::Wait => return false,
        };
        match self.state {
            Data => match c {
                Some('&') => {
                    self.return_state = Data;
                    self.state = CharacterReference;
                }
                Some('<') => self.state = TagOpen,
                Some(c) => self.emit_char(c),
                None => self.emit_eof(),
            },
            Rcdata => match c {
                Some('&') => {
                    self.return_state = Rcdata;
                    self.state = CharacterReference;
                }
                Some('<') => self.state = RcdataLessThanSign,
                Some('\0') => self.emit_char('\u{FFFD}'),
                Some(c) => self.emit_char(c),
                None => self.emit_eof(),
            },
            Rawtext => match c {
                Some('<') => self.state = RawtextLessThanSign,
                Some('\0') => self.emit_char('\u{FFFD}'),
                Some(c) => self.emit_char(c),
                None => self.emit_eof(),
            },
            ScriptData => match c {
                Some('<') => self.state = ScriptDataLessThanSign,
                Some('\0') => self.emit_char('\u{FFFD}'),
                Some(c) => self.emit_char(c),
                None => self.emit_eof(),
            },
            Plaintext => match c {
                Some('\0') => self.emit_char('\u{FFFD}'),
                Some(c) => self.emit_char(c),
                None => self.emit_eof(),
            },
            TagOpen => match c {
                Some('!') => self.state = MarkupDeclarationOpen,
                Some('/') => self.state = EndTagOpen,
                Some(ch) if ch.is_ascii_alphabetic() => {
                    self.new_tag(false);
                    self.reconsume(input, c, TagName);
                }
                Some('?') => {
                    self.temp.clear();
                    self.state = ProcessingInstructionOpen;
                }
                None => {
                    self.emit_char('<');
                    self.emit_eof();
                }
                Some(_) => {
                    self.emit_char('<');
                    self.reconsume(input, c, Data);
                }
            },
            EndTagOpen => match c {
                Some(ch) if ch.is_ascii_alphabetic() => {
                    self.new_tag(true);
                    self.reconsume(input, c, TagName);
                }
                Some('>') => self.state = Data,
                None => {
                    self.emit_str("</");
                    self.emit_eof();
                }
                Some(_) => {
                    self.comment.clear();
                    self.reconsume(input, c, BogusComment);
                }
            },
            TagName => match c {
                Some(ch) if is_ws(ch) => self.state = BeforeAttributeName,
                Some('/') => self.state = SelfClosingStartTag,
                Some('>') => {
                    self.state = Data;
                    self.emit_tag();
                }
                Some('\0') => self.tag_name.push('\u{FFFD}'),
                Some(ch) => self.tag_name.push(ch.to_ascii_lowercase()),
                None => self.emit_eof(),
            },
            RcdataLessThanSign => match c {
                Some('/') => {
                    self.temp.clear();
                    self.state = RcdataEndTagOpen;
                }
                _ => {
                    self.emit_char('<');
                    self.reconsume(input, c, Rcdata);
                }
            },
            RcdataEndTagOpen => match c {
                Some(ch) if ch.is_ascii_alphabetic() => {
                    self.new_tag(true);
                    self.reconsume(input, c, RcdataEndTagName);
                }
                _ => {
                    self.emit_str("</");
                    self.reconsume(input, c, Rcdata);
                }
            },
            RcdataEndTagName => self.text_end_tag_name(input, c, Rcdata),
            RawtextLessThanSign => match c {
                Some('/') => {
                    self.temp.clear();
                    self.state = RawtextEndTagOpen;
                }
                _ => {
                    self.emit_char('<');
                    self.reconsume(input, c, Rawtext);
                }
            },
            RawtextEndTagOpen => match c {
                Some(ch) if ch.is_ascii_alphabetic() => {
                    self.new_tag(true);
                    self.reconsume(input, c, RawtextEndTagName);
                }
                _ => {
                    self.emit_str("</");
                    self.reconsume(input, c, Rawtext);
                }
            },
            RawtextEndTagName => self.text_end_tag_name(input, c, Rawtext),
            ScriptDataLessThanSign => match c {
                Some('/') => {
                    self.temp.clear();
                    self.state = ScriptDataEndTagOpen;
                }
                Some('!') => {
                    self.state = ScriptDataEscapeStart;
                    self.emit_str("<!");
                }
                _ => {
                    self.emit_char('<');
                    self.reconsume(input, c, ScriptData);
                }
            },
            ScriptDataEndTagOpen => match c {
                Some(ch) if ch.is_ascii_alphabetic() => {
                    self.new_tag(true);
                    self.reconsume(input, c, ScriptDataEndTagName);
                }
                _ => {
                    self.emit_str("</");
                    self.reconsume(input, c, ScriptData);
                }
            },
            ScriptDataEndTagName => self.text_end_tag_name(input, c, ScriptData),
            ScriptDataEscapeStart => match c {
                Some('-') => {
                    self.state = ScriptDataEscapeStartDash;
                    self.emit_char('-');
                }
                _ => self.reconsume(input, c, ScriptData),
            },
            ScriptDataEscapeStartDash => match c {
                Some('-') => {
                    self.state = ScriptDataEscapedDashDash;
                    self.emit_char('-');
                }
                _ => self.reconsume(input, c, ScriptData),
            },
            ScriptDataEscaped => match c {
                Some('-') => {
                    self.state = ScriptDataEscapedDash;
                    self.emit_char('-');
                }
                Some('<') => self.state = ScriptDataEscapedLessThanSign,
                Some('\0') => self.emit_char('\u{FFFD}'),
                Some(ch) => self.emit_char(ch),
                None => self.emit_eof(),
            },
            ScriptDataEscapedDash => match c {
                Some('-') => {
                    self.state = ScriptDataEscapedDashDash;
                    self.emit_char('-');
                }
                Some('<') => self.state = ScriptDataEscapedLessThanSign,
                Some('\0') => {
                    self.state = ScriptDataEscaped;
                    self.emit_char('\u{FFFD}');
                }
                Some(ch) => {
                    self.state = ScriptDataEscaped;
                    self.emit_char(ch);
                }
                None => self.emit_eof(),
            },
            ScriptDataEscapedDashDash => match c {
                Some('-') => self.emit_char('-'),
                Some('<') => self.state = ScriptDataEscapedLessThanSign,
                Some('>') => {
                    self.state = ScriptData;
                    self.emit_char('>');
                }
                Some('\0') => {
                    self.state = ScriptDataEscaped;
                    self.emit_char('\u{FFFD}');
                }
                Some(ch) => {
                    self.state = ScriptDataEscaped;
                    self.emit_char(ch);
                }
                None => self.emit_eof(),
            },
            ScriptDataEscapedLessThanSign => match c {
                Some('/') => {
                    self.temp.clear();
                    self.state = ScriptDataEscapedEndTagOpen;
                }
                Some(ch) if ch.is_ascii_alphabetic() => {
                    self.temp.clear();
                    self.emit_char('<');
                    self.reconsume(input, c, ScriptDataDoubleEscapeStart);
                }
                _ => {
                    self.emit_char('<');
                    self.reconsume(input, c, ScriptDataEscaped);
                }
            },
            ScriptDataEscapedEndTagOpen => match c {
                Some(ch) if ch.is_ascii_alphabetic() => {
                    self.new_tag(true);
                    self.reconsume(input, c, ScriptDataEscapedEndTagName);
                }
                _ => {
                    self.emit_str("</");
                    self.reconsume(input, c, ScriptDataEscaped);
                }
            },
            ScriptDataEscapedEndTagName => self.text_end_tag_name(input, c, ScriptDataEscaped),
            ScriptDataDoubleEscapeStart => match c {
                Some(ch) if is_ws(ch) || ch == '/' || ch == '>' => {
                    self.state = if self.temp == "script" {
                        ScriptDataDoubleEscaped
                    } else {
                        ScriptDataEscaped
                    };
                    self.emit_char(ch);
                }
                Some(ch) if ch.is_ascii_alphabetic() => {
                    self.temp.push(ch.to_ascii_lowercase());
                    self.emit_char(ch);
                }
                _ => self.reconsume(input, c, ScriptDataEscaped),
            },
            ScriptDataDoubleEscaped => match c {
                Some('-') => {
                    self.state = ScriptDataDoubleEscapedDash;
                    self.emit_char('-');
                }
                Some('<') => {
                    self.state = ScriptDataDoubleEscapedLessThanSign;
                    self.emit_char('<');
                }
                Some('\0') => self.emit_char('\u{FFFD}'),
                Some(ch) => self.emit_char(ch),
                None => self.emit_eof(),
            },
            ScriptDataDoubleEscapedDash => match c {
                Some('-') => {
                    self.state = ScriptDataDoubleEscapedDashDash;
                    self.emit_char('-');
                }
                Some('<') => {
                    self.state = ScriptDataDoubleEscapedLessThanSign;
                    self.emit_char('<');
                }
                Some('\0') => {
                    self.state = ScriptDataDoubleEscaped;
                    self.emit_char('\u{FFFD}');
                }
                Some(ch) => {
                    self.state = ScriptDataDoubleEscaped;
                    self.emit_char(ch);
                }
                None => self.emit_eof(),
            },
            ScriptDataDoubleEscapedDashDash => match c {
                Some('-') => self.emit_char('-'),
                Some('<') => {
                    self.state = ScriptDataDoubleEscapedLessThanSign;
                    self.emit_char('<');
                }
                Some('>') => {
                    self.state = ScriptData;
                    self.emit_char('>');
                }
                Some('\0') => {
                    self.state = ScriptDataDoubleEscaped;
                    self.emit_char('\u{FFFD}');
                }
                Some(ch) => {
                    self.state = ScriptDataDoubleEscaped;
                    self.emit_char(ch);
                }
                None => self.emit_eof(),
            },
            ScriptDataDoubleEscapedLessThanSign => match c {
                Some('/') => {
                    self.temp.clear();
                    self.state = ScriptDataDoubleEscapeEnd;
                    self.emit_char('/');
                }
                _ => self.reconsume(input, c, ScriptDataDoubleEscaped),
            },
            ScriptDataDoubleEscapeEnd => match c {
                Some(ch) if is_ws(ch) || ch == '/' || ch == '>' => {
                    self.state = if self.temp == "script" {
                        ScriptDataEscaped
                    } else {
                        ScriptDataDoubleEscaped
                    };
                    self.emit_char(ch);
                }
                Some(ch) if ch.is_ascii_alphabetic() => {
                    self.temp.push(ch.to_ascii_lowercase());
                    self.emit_char(ch);
                }
                _ => self.reconsume(input, c, ScriptDataDoubleEscaped),
            },
            BeforeAttributeName => match c {
                Some(ch) if is_ws(ch) => {}
                Some('/') | Some('>') | None => self.reconsume(input, c, AfterAttributeName),
                Some('=') => {
                    self.new_attr("=");
                    self.state = AttributeName;
                }
                Some(_) => {
                    self.new_attr("");
                    self.reconsume(input, c, AttributeName);
                }
            },
            AttributeName => match c {
                Some(ch) if is_ws(ch) => self.reconsume(input, c, AfterAttributeName),
                Some('/') | Some('>') | None => self.reconsume(input, c, AfterAttributeName),
                Some('=') => self.state = BeforeAttributeValue,
                Some('\0') => self.attr_name_push('\u{FFFD}'),
                Some(ch) => self.attr_name_push(ch.to_ascii_lowercase()),
            },
            AfterAttributeName => match c {
                Some(ch) if is_ws(ch) => {}
                Some('/') => self.state = SelfClosingStartTag,
                Some('=') => self.state = BeforeAttributeValue,
                Some('>') => {
                    self.state = Data;
                    self.emit_tag();
                }
                None => self.emit_eof(),
                Some(_) => {
                    self.new_attr("");
                    self.reconsume(input, c, AttributeName);
                }
            },
            BeforeAttributeValue => match c {
                Some(ch) if is_ws(ch) => {}
                Some('"') => self.state = AttributeValueDoubleQuoted,
                Some('\'') => self.state = AttributeValueSingleQuoted,
                Some('>') => {
                    self.state = Data;
                    self.emit_tag();
                }
                _ => self.reconsume(input, c, AttributeValueUnquoted),
            },
            AttributeValueDoubleQuoted => match c {
                Some('"') => self.state = AfterAttributeValueQuoted,
                Some('&') => {
                    self.return_state = AttributeValueDoubleQuoted;
                    self.state = CharacterReference;
                }
                Some('\0') => self.attr_value_push('\u{FFFD}'),
                Some(ch) => self.attr_value_push(ch),
                None => self.emit_eof(),
            },
            AttributeValueSingleQuoted => match c {
                Some('\'') => self.state = AfterAttributeValueQuoted,
                Some('&') => {
                    self.return_state = AttributeValueSingleQuoted;
                    self.state = CharacterReference;
                }
                Some('\0') => self.attr_value_push('\u{FFFD}'),
                Some(ch) => self.attr_value_push(ch),
                None => self.emit_eof(),
            },
            AttributeValueUnquoted => match c {
                Some(ch) if is_ws(ch) => self.state = BeforeAttributeName,
                Some('&') => {
                    self.return_state = AttributeValueUnquoted;
                    self.state = CharacterReference;
                }
                Some('>') => {
                    self.state = Data;
                    self.emit_tag();
                }
                Some('\0') => self.attr_value_push('\u{FFFD}'),
                Some(ch) => self.attr_value_push(ch),
                None => self.emit_eof(),
            },
            AfterAttributeValueQuoted => match c {
                Some(ch) if is_ws(ch) => self.state = BeforeAttributeName,
                Some('/') => self.state = SelfClosingStartTag,
                Some('>') => {
                    self.state = Data;
                    self.emit_tag();
                }
                None => self.emit_eof(),
                Some(_) => self.reconsume(input, c, BeforeAttributeName),
            },
            SelfClosingStartTag => match c {
                Some('>') => {
                    self.tag_self_closing = true;
                    self.state = Data;
                    self.emit_tag();
                }
                None => self.emit_eof(),
                Some(_) => self.reconsume(input, c, BeforeAttributeName),
            },
            BogusComment => match c {
                Some('>') => {
                    self.state = Data;
                    self.emit_comment();
                }
                None => {
                    self.emit_comment();
                    self.emit_eof();
                }
                Some('\0') => self.comment.push('\u{FFFD}'),
                Some(ch) => self.comment.push(ch),
            },
            CommentStart => match c {
                Some('-') => self.state = CommentStartDash,
                Some('>') => {
                    self.state = Data;
                    self.emit_comment();
                }
                _ => self.reconsume(input, c, Comment),
            },
            CommentStartDash => match c {
                Some('-') => self.state = CommentEnd,
                Some('>') => {
                    self.state = Data;
                    self.emit_comment();
                }
                None => {
                    self.emit_comment();
                    self.emit_eof();
                }
                Some(_) => {
                    self.comment.push('-');
                    self.reconsume(input, c, Comment);
                }
            },
            Comment => match c {
                Some('<') => {
                    self.comment.push('<');
                    self.state = CommentLessThanSign;
                }
                Some('-') => self.state = CommentEndDash,
                Some('\0') => self.comment.push('\u{FFFD}'),
                Some(ch) => self.comment.push(ch),
                None => {
                    self.emit_comment();
                    self.emit_eof();
                }
            },
            CommentLessThanSign => match c {
                Some('!') => {
                    self.comment.push('!');
                    self.state = CommentLessThanSignBang;
                }
                Some('<') => self.comment.push('<'),
                _ => self.reconsume(input, c, Comment),
            },
            CommentLessThanSignBang => match c {
                Some('-') => self.state = CommentLessThanSignBangDash,
                _ => self.reconsume(input, c, Comment),
            },
            CommentLessThanSignBangDash => match c {
                Some('-') => self.state = CommentLessThanSignBangDashDash,
                _ => self.reconsume(input, c, CommentEndDash),
            },
            CommentLessThanSignBangDashDash => self.reconsume(input, c, CommentEnd),
            CommentEndDash => match c {
                Some('-') => self.state = CommentEnd,
                None => {
                    self.emit_comment();
                    self.emit_eof();
                }
                Some(_) => {
                    self.comment.push('-');
                    self.reconsume(input, c, Comment);
                }
            },
            CommentEnd => match c {
                Some('>') => {
                    self.state = Data;
                    self.emit_comment();
                }
                Some('!') => self.state = CommentEndBang,
                Some('-') => self.comment.push('-'),
                None => {
                    self.emit_comment();
                    self.emit_eof();
                }
                Some(_) => {
                    self.comment.push_str("--");
                    self.reconsume(input, c, Comment);
                }
            },
            CommentEndBang => match c {
                Some('-') => {
                    self.comment.push_str("--!");
                    self.state = CommentEndDash;
                }
                Some('>') => {
                    self.state = Data;
                    self.emit_comment();
                }
                None => {
                    self.emit_comment();
                    self.emit_eof();
                }
                Some(_) => {
                    self.comment.push_str("--!");
                    self.reconsume(input, c, Comment);
                }
            },
            Doctype => match c {
                Some(ch) if is_ws(ch) => self.state = BeforeDoctypeName,
                None => {
                    self.new_doctype();
                    self.doctype_eof();
                }
                Some(_) => self.reconsume(input, c, BeforeDoctypeName),
            },
            BeforeDoctypeName => match c {
                Some(ch) if is_ws(ch) => {}
                Some('\0') => {
                    self.new_doctype();
                    self.doctype.name = Some("\u{FFFD}".to_string());
                    self.state = DoctypeName;
                }
                Some('>') => {
                    self.new_doctype();
                    self.state = Data;
                    self.doctype_quirks_emit();
                }
                None => {
                    self.new_doctype();
                    self.doctype_eof();
                }
                Some(ch) => {
                    self.new_doctype();
                    self.doctype.name = Some(ch.to_ascii_lowercase().to_string());
                    self.state = DoctypeName;
                }
            },
            DoctypeName => match c {
                Some(ch) if is_ws(ch) => self.state = AfterDoctypeName,
                Some('>') => {
                    self.state = Data;
                    self.emit_doctype();
                }
                Some(ch) => {
                    let ch = if ch == '\0' {
                        '\u{FFFD}'
                    } else {
                        ch.to_ascii_lowercase()
                    };
                    self.doctype.name.get_or_insert_with(String::new).push(ch);
                }
                None => self.doctype_eof(),
            },
            AfterDoctypePublicKeyword => match c {
                Some(ch) if is_ws(ch) => self.state = BeforeDoctypePublicIdentifier,
                Some('"') => {
                    self.doctype.public_id = Some(String::new());
                    self.state = DoctypePublicIdentifierDoubleQuoted;
                }
                Some('\'') => {
                    self.doctype.public_id = Some(String::new());
                    self.state = DoctypePublicIdentifierSingleQuoted;
                }
                Some('>') => {
                    self.state = Data;
                    self.doctype_quirks_emit();
                }
                None => self.doctype_eof(),
                Some(_) => {
                    self.doctype.force_quirks = true;
                    self.reconsume(input, c, BogusDoctype);
                }
            },
            BeforeDoctypePublicIdentifier => match c {
                Some(ch) if is_ws(ch) => {}
                Some('"') => {
                    self.doctype.public_id = Some(String::new());
                    self.state = DoctypePublicIdentifierDoubleQuoted;
                }
                Some('\'') => {
                    self.doctype.public_id = Some(String::new());
                    self.state = DoctypePublicIdentifierSingleQuoted;
                }
                Some('>') => {
                    self.state = Data;
                    self.doctype_quirks_emit();
                }
                None => self.doctype_eof(),
                Some(_) => {
                    self.doctype.force_quirks = true;
                    self.reconsume(input, c, BogusDoctype);
                }
            },
            DoctypePublicIdentifierDoubleQuoted | DoctypePublicIdentifierSingleQuoted => {
                let quote = if self.state == DoctypePublicIdentifierDoubleQuoted {
                    '"'
                } else {
                    '\''
                };
                match c {
                    Some(ch) if ch == quote => self.state = AfterDoctypePublicIdentifier,
                    Some('>') => {
                        self.state = Data;
                        self.doctype_quirks_emit();
                    }
                    Some(ch) => {
                        let ch = if ch == '\0' { '\u{FFFD}' } else { ch };
                        self.doctype
                            .public_id
                            .get_or_insert_with(String::new)
                            .push(ch);
                    }
                    None => self.doctype_eof(),
                }
            }
            AfterDoctypePublicIdentifier => match c {
                Some(ch) if is_ws(ch) => self.state = BetweenDoctypePublicAndSystemIdentifiers,
                Some('>') => {
                    self.state = Data;
                    self.emit_doctype();
                }
                Some('"') => {
                    self.doctype.system_id = Some(String::new());
                    self.state = DoctypeSystemIdentifierDoubleQuoted;
                }
                Some('\'') => {
                    self.doctype.system_id = Some(String::new());
                    self.state = DoctypeSystemIdentifierSingleQuoted;
                }
                None => self.doctype_eof(),
                Some(_) => {
                    self.doctype.force_quirks = true;
                    self.reconsume(input, c, BogusDoctype);
                }
            },
            BetweenDoctypePublicAndSystemIdentifiers => match c {
                Some(ch) if is_ws(ch) => {}
                Some('>') => {
                    self.state = Data;
                    self.emit_doctype();
                }
                Some('"') => {
                    self.doctype.system_id = Some(String::new());
                    self.state = DoctypeSystemIdentifierDoubleQuoted;
                }
                Some('\'') => {
                    self.doctype.system_id = Some(String::new());
                    self.state = DoctypeSystemIdentifierSingleQuoted;
                }
                None => self.doctype_eof(),
                Some(_) => {
                    self.doctype.force_quirks = true;
                    self.reconsume(input, c, BogusDoctype);
                }
            },
            AfterDoctypeSystemKeyword => match c {
                Some(ch) if is_ws(ch) => self.state = BeforeDoctypeSystemIdentifier,
                Some('"') => {
                    self.doctype.system_id = Some(String::new());
                    self.state = DoctypeSystemIdentifierDoubleQuoted;
                }
                Some('\'') => {
                    self.doctype.system_id = Some(String::new());
                    self.state = DoctypeSystemIdentifierSingleQuoted;
                }
                Some('>') => {
                    self.state = Data;
                    self.doctype_quirks_emit();
                }
                None => self.doctype_eof(),
                Some(_) => {
                    self.doctype.force_quirks = true;
                    self.reconsume(input, c, BogusDoctype);
                }
            },
            BeforeDoctypeSystemIdentifier => match c {
                Some(ch) if is_ws(ch) => {}
                Some('"') => {
                    self.doctype.system_id = Some(String::new());
                    self.state = DoctypeSystemIdentifierDoubleQuoted;
                }
                Some('\'') => {
                    self.doctype.system_id = Some(String::new());
                    self.state = DoctypeSystemIdentifierSingleQuoted;
                }
                Some('>') => {
                    self.state = Data;
                    self.doctype_quirks_emit();
                }
                None => self.doctype_eof(),
                Some(_) => {
                    self.doctype.force_quirks = true;
                    self.reconsume(input, c, BogusDoctype);
                }
            },
            DoctypeSystemIdentifierDoubleQuoted | DoctypeSystemIdentifierSingleQuoted => {
                let quote = if self.state == DoctypeSystemIdentifierDoubleQuoted {
                    '"'
                } else {
                    '\''
                };
                match c {
                    Some(ch) if ch == quote => self.state = AfterDoctypeSystemIdentifier,
                    Some('>') => {
                        self.state = Data;
                        self.doctype_quirks_emit();
                    }
                    Some(ch) => {
                        let ch = if ch == '\0' { '\u{FFFD}' } else { ch };
                        self.doctype
                            .system_id
                            .get_or_insert_with(String::new)
                            .push(ch);
                    }
                    None => self.doctype_eof(),
                }
            }
            AfterDoctypeSystemIdentifier => match c {
                Some(ch) if is_ws(ch) => {}
                Some('>') => {
                    self.state = Data;
                    self.emit_doctype();
                }
                None => self.doctype_eof(),
                Some(_) => self.reconsume(input, c, BogusDoctype),
            },
            BogusDoctype => match c {
                Some('>') => {
                    self.state = Data;
                    self.emit_doctype();
                }
                None => {
                    self.emit_doctype();
                    self.emit_eof();
                }
                Some(_) => {}
            },
            CdataSection => match c {
                Some(']') => self.state = CdataSectionBracket,
                Some(ch) => self.emit_char(ch),
                None => self.emit_eof(),
            },
            CdataSectionBracket => match c {
                Some(']') => self.state = CdataSectionEnd,
                _ => {
                    self.emit_char(']');
                    self.reconsume(input, c, CdataSection);
                }
            },
            CdataSectionEnd => match c {
                Some(']') => self.emit_char(']'),
                Some('>') => self.state = Data,
                _ => {
                    self.emit_str("]]");
                    self.reconsume(input, c, CdataSection);
                }
            },
            ProcessingInstructionOpen => match c {
                Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {
                    self.reconsume(input, c, ProcessingInstructionTarget)
                }
                None => self.emit_eof(),
                Some(_) => {
                    self.temp_to_comment();
                    self.reconsume(input, c, BogusComment);
                }
            },
            ProcessingInstructionTarget => match c {
                Some(ch) if is_ws(ch) || ch == '?' || ch == '>' => {
                    if self.temp.eq_ignore_ascii_case("xml")
                        || self.temp.eq_ignore_ascii_case("xml-stylesheet")
                    {
                        self.temp_to_comment();
                        self.reconsume(input, c, BogusComment);
                    } else {
                        self.pi_target = std::mem::take(&mut self.temp);
                        self.pi_data.clear();
                        self.reconsume(input, c, AfterProcessingInstructionTarget);
                    }
                }
                Some(ch) if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' => {
                    self.temp.push(ch)
                }
                None => self.emit_eof(),
                Some(_) => {
                    self.temp_to_comment();
                    self.reconsume(input, c, BogusComment);
                }
            },
            AfterProcessingInstructionTarget => match c {
                Some(ch) if is_ws(ch) => {}
                _ => self.reconsume(input, c, ProcessingInstructionData),
            },
            ProcessingInstructionData => match c {
                Some('?') => self.state = ProcessingInstructionQuestionable,
                Some('>') => {
                    self.state = Data;
                    self.emit_pi();
                }
                Some(ch) => self.pi_data.push(ch),
                None => self.emit_eof(),
            },
            ProcessingInstructionQuestionable => match c {
                Some('>') => {
                    self.state = Data;
                    self.emit_pi();
                }
                None => self.emit_eof(),
                Some(_) => {
                    self.pi_data.push('?');
                    self.reconsume(input, c, ProcessingInstructionData);
                }
            },
            CharacterReference => {
                self.temp.clear();
                self.temp.push('&');
                match c {
                    Some(ch) if ch.is_ascii_alphanumeric() => {
                        self.reconsume(input, c, NamedCharacterReference)
                    }
                    Some('#') => {
                        self.temp.push('#');
                        self.state = NumericCharacterReference;
                    }
                    _ => {
                        self.flush_char_ref();
                        let back = self.return_state;
                        self.reconsume(input, c, back);
                    }
                }
            }
            AmbiguousAmpersand => match c {
                Some(ch) if ch.is_ascii_alphanumeric() => {
                    if self.in_attribute() {
                        self.attr_value_push(ch);
                    } else {
                        self.emit_char(ch);
                    }
                }
                _ => {
                    let back = self.return_state;
                    self.reconsume(input, c, back);
                }
            },
            NumericCharacterReference => {
                self.char_ref_code = 0;
                match c {
                    Some(ch @ ('x' | 'X')) => {
                        self.temp.push(ch);
                        self.state = HexadecimalCharacterReferenceStart;
                    }
                    Some(ch) if ch.is_ascii_digit() => {
                        self.reconsume(input, c, DecimalCharacterReference)
                    }
                    _ => {
                        self.flush_char_ref();
                        let back = self.return_state;
                        self.reconsume(input, c, back);
                    }
                }
            }
            HexadecimalCharacterReferenceStart => match c {
                Some(ch) if ch.is_ascii_hexdigit() => {
                    self.reconsume(input, c, HexadecimalCharacterReference)
                }
                _ => {
                    self.flush_char_ref();
                    let back = self.return_state;
                    self.reconsume(input, c, back);
                }
            },
            HexadecimalCharacterReference => match c {
                Some(ch) if ch.is_ascii_hexdigit() => {
                    let digit = ch.to_digit(16).unwrap_or(0);
                    self.char_ref_code = (self.char_ref_code * 16 + digit).min(0x11_0000);
                }
                Some(';') => self.state = NumericCharacterReferenceEnd,
                _ => self.reconsume(input, c, NumericCharacterReferenceEnd),
            },
            DecimalCharacterReference => match c {
                Some(ch) if ch.is_ascii_digit() => {
                    let digit = ch.to_digit(10).unwrap_or(0);
                    self.char_ref_code = (self.char_ref_code * 10 + digit).min(0x11_0000);
                }
                Some(';') => self.state = NumericCharacterReferenceEnd,
                _ => self.reconsume(input, c, NumericCharacterReferenceEnd),
            },
            MarkupDeclarationOpen
            | NamedCharacterReference
            | AfterDoctypeName
            | NumericCharacterReferenceEnd => unreachable!("handled before consuming"),
        }
        true
    }

    /// HTML §13.2.5.42.
    fn markup_declaration_open(&mut self, input: &mut InputStream) -> bool {
        let dashes = input.lookahead("--", false);
        if dashes == Some(true) {
            input.skip(2);
            self.comment.clear();
            self.state = State::CommentStart;
            return true;
        }
        let doctype = input.lookahead("DOCTYPE", true);
        if doctype == Some(true) {
            input.skip(7);
            self.state = State::Doctype;
            return true;
        }
        let cdata = input.lookahead("[CDATA[", false);
        if cdata == Some(true) {
            input.skip(7);
            if self.allow_cdata {
                self.state = State::CdataSection;
            } else {
                self.comment = "[CDATA[".to_string();
                self.state = State::BogusComment;
            }
            return true;
        }
        if dashes.is_none() || doctype.is_none() || cdata.is_none() {
            return false;
        }
        self.comment.clear();
        self.state = State::BogusComment;
        true
    }

    /// HTML §13.2.5.56: the first character is consumed here, and "PUBLIC"
    /// or "SYSTEM" is looked for from it.
    fn after_doctype_name(&mut self, input: &mut InputStream) -> bool {
        match input.peek(0) {
            Next::Wait => false,
            Next::Eof => {
                self.doctype_eof();
                true
            }
            Next::Char(c) if is_ws(c) => {
                input.skip(1);
                true
            }
            Next::Char('>') => {
                input.skip(1);
                self.state = State::Data;
                self.emit_doctype();
                true
            }
            Next::Char(_) => {
                let public = input.lookahead("PUBLIC", true);
                if public == Some(true) {
                    input.skip(6);
                    self.state = State::AfterDoctypePublicKeyword;
                    return true;
                }
                let system = input.lookahead("SYSTEM", true);
                if system == Some(true) {
                    input.skip(6);
                    self.state = State::AfterDoctypeSystemKeyword;
                    return true;
                }
                if public.is_none() || system.is_none() {
                    return false;
                }
                // Reconsume the (not yet consumed) character in bogus DOCTYPE.
                self.doctype.force_quirks = true;
                self.state = State::BogusDoctype;
                true
            }
        }
    }

    /// HTML §13.2.5.78.
    fn named_character_reference(&mut self, input: &mut InputStream) -> bool {
        let Ok(found) = match_named_reference(input) else {
            return false;
        };
        let Some((len, index)) = found else {
            self.flush_char_ref();
            self.state = State::AmbiguousAmpersand;
            return true;
        };
        let (name, first, second) = ENTITIES[index];
        if self.in_attribute() && !name.ends_with(';') {
            match input.peek(len) {
                Next::Wait => return false,
                Next::Char(c) if c == '=' || c.is_ascii_alphanumeric() => {
                    input.skip(len);
                    self.temp.push_str(name);
                    self.flush_char_ref();
                    self.state = self.return_state;
                    return true;
                }
                _ => {}
            }
        }
        input.skip(len);
        self.temp.clear();
        self.temp.extend(char::from_u32(first));
        if second != 0 {
            self.temp.extend(char::from_u32(second));
        }
        self.flush_char_ref();
        self.state = self.return_state;
        true
    }

    /// HTML §13.2.5.84.
    fn numeric_character_reference_end(&mut self) {
        let mut code = self.char_ref_code;
        if code == 0 || code > 0x10FFFF || (0xD800..=0xDFFF).contains(&code) {
            code = 0xFFFD;
        } else if let Some(replacement) = c1_replacement(code) {
            code = replacement;
        }
        self.temp.clear();
        self.temp.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
        self.flush_char_ref();
        self.state = self.return_state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(text: &str) -> Vec<Token> {
        let mut input = InputStream::new();
        input.push(text);
        input.close();
        let mut tokenizer = Tokenizer::new();
        let mut out = Vec::new();
        while let Some(token) = tokenizer.next_token(&mut input) {
            let eof = token == Token::Eof;
            out.push(token);
            if eof {
                break;
            }
        }
        out
    }

    fn text_of(tokens: &[Token]) -> String {
        tokens
            .iter()
            .filter_map(|t| match t {
                Token::Char(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn named_references_take_the_longest_match() {
        assert_eq!(text_of(&tokens("I'm &notit; I")), "I'm \u{AC}it; I");
        assert_eq!(text_of(&tokens("&notin;x")), "\u{2209}x");
        assert_eq!(text_of(&tokens("&amp")), "&");
        assert_eq!(text_of(&tokens("&xyz;")), "&xyz;");
        assert_eq!(text_of(&tokens("&NotEqualTilde;")), "\u{2242}\u{338}");
    }

    #[test]
    fn references_in_attributes_keep_legacy_text() {
        let toks = tokens("<a href='?x=1&notit=2&amp;y&lt'>");
        let Token::StartTag(tag) = &toks[0] else {
            panic!("start tag expected: {toks:?}");
        };
        assert_eq!(tag.attr("href"), Some("?x=1&notit=2&y<"));
    }

    #[test]
    fn numeric_references_are_replaced() {
        assert_eq!(
            text_of(&tokens("&#x80;&#0;&#x110000;&#65&#xD800;")),
            "\u{20AC}\u{FFFD}\u{FFFD}A\u{FFFD}"
        );
    }

    #[test]
    fn tags_attributes_and_duplicates() {
        let toks = tokens("<DIV Id=a id=b CLASS = \"c\" data-x/>");
        let Token::StartTag(tag) = &toks[0] else {
            panic!("start tag expected");
        };
        assert_eq!(tag.name, "div");
        assert!(tag.self_closing);
        let names: Vec<&str> = tag.attrs.iter().map(|a| a.local.as_str()).collect();
        assert_eq!(names, ["id", "class", "data-x"]);
        assert_eq!(tag.attr("id"), Some("a"));
    }

    #[test]
    fn comments_doctypes_and_processing_instructions() {
        assert_eq!(tokens("<!--a--!>")[0], Token::Comment("a".to_string()));
        assert_eq!(tokens("<!-->")[0], Token::Comment(String::new()));
        assert_eq!(tokens("</ x>")[0], Token::Comment(" x".to_string()));
        assert_eq!(
            tokens("<?xml v?>")[0],
            Token::Comment("?xml v?".to_string())
        );
        assert_eq!(
            tokens("<?target  some data?>")[0],
            Token::ProcessingInstruction {
                target: "target".to_string(),
                data: "some data".to_string()
            }
        );
        assert_eq!(tokens("<?1>")[0], Token::Comment("?1".to_string()));
        let Token::Doctype(d) = &tokens("<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\">")[0]
        else {
            panic!("doctype expected");
        };
        assert_eq!(d.name.as_deref(), Some("html"));
        assert_eq!(d.public_id.as_deref(), Some("-//W3C//DTD HTML 4.01//EN"));
        assert_eq!(d.system_id, None);
        assert!(!d.force_quirks);
        let Token::Doctype(d) = &tokens("<!DOCTYPE html bogus>")[0] else {
            panic!("doctype expected");
        };
        assert!(d.force_quirks);
    }

    #[test]
    fn rawtext_ends_only_at_appropriate_end_tag() {
        let mut input = InputStream::new();
        input.push("<style>a</b></style>");
        input.close();
        let mut tokenizer = Tokenizer::new();
        assert!(matches!(
            tokenizer.next_token(&mut input),
            Some(Token::StartTag(_))
        ));
        tokenizer.set_state(State::Rawtext);
        let mut text = String::new();
        loop {
            match tokenizer.next_token(&mut input) {
                Some(Token::Char(c)) => text.push(c),
                Some(Token::EndTag(tag)) => {
                    assert_eq!(tag.name, "style");
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(text, "a</b>");
    }

    #[test]
    fn partial_input_resumes() {
        let mut input = InputStream::new();
        let mut tokenizer = Tokenizer::new();
        input.push("<!DOC");
        assert_eq!(tokenizer.next_token(&mut input), None);
        input.push("TYPE html><p cl");
        assert!(matches!(
            tokenizer.next_token(&mut input),
            Some(Token::Doctype(_))
        ));
        assert_eq!(tokenizer.next_token(&mut input), None);
        input.push("ass=x>&am");
        assert!(matches!(
            tokenizer.next_token(&mut input),
            Some(Token::StartTag(_))
        ));
        assert_eq!(tokenizer.next_token(&mut input), None);
        input.push("p;");
        input.close();
        assert_eq!(tokenizer.next_token(&mut input), Some(Token::Char('&')));
        assert_eq!(tokenizer.next_token(&mut input), Some(Token::Eof));
    }

    #[test]
    fn cdata_only_when_allowed() {
        let mut input = InputStream::new();
        input.push("<![CDATA[x]]>");
        input.close();
        let mut tokenizer = Tokenizer::new();
        tokenizer.allow_cdata = true;
        assert_eq!(tokenizer.next_token(&mut input), Some(Token::Char('x')));
        assert_eq!(
            tokens("<![CDATA[x]]>")[0],
            Token::Comment("[CDATA[x]]".to_string())
        );
    }
}
