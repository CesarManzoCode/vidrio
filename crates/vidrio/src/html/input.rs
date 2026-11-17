//! The input stream of the HTML parser (HTML §13.2.3.5) with its insertion
//! point (used by `document.write`).

/// What the tokenizer sees next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    Char(char),
    /// The end of the input stream.
    Eof,
    /// No character is available yet: more input may still come.
    Wait,
}

/// The characters given to the parser, newlines already normalized.
#[derive(Clone, Debug, Default)]
pub struct InputStream {
    chars: Vec<char>,
    pos: usize,
    /// The end of the stream has been seen (no more input will be fed).
    ended: bool,
    /// Index of the character just after the insertion point, if defined.
    insertion_point: Option<usize>,
    /// The tokenizer stops at the insertion point instead of the end.
    stop_at_insertion_point: bool,
    /// The last fed chunk ended with U+000D, so a leading U+000A of the next
    /// one belongs to the same newline.
    pending_cr: bool,
}

/// Normalizes newlines (CR LF and lone CR become LF) of `text` into `out`.
/// Returns whether `text` ended with CR.
fn normalize_into(text: &str, out: &mut Vec<char>, mut skip_lf: bool) -> bool {
    let mut last_cr = false;
    for c in text.chars() {
        if skip_lf {
            skip_lf = false;
            if c == '\n' {
                continue;
            }
        }
        if c == '\r' {
            out.push('\n');
            last_cr = true;
            skip_lf = true;
        } else {
            out.push(c);
            last_cr = false;
        }
    }
    last_cr
}

impl InputStream {
    pub fn new() -> InputStream {
        InputStream::default()
    }

    /// Appends decoded text at the end of the stream.
    pub fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let skip = self.pending_cr;
        self.pending_cr = normalize_into(text, &mut self.chars, skip);
    }

    /// Marks the end of the stream (the "EOF" character).
    pub fn close(&mut self) {
        self.ended = true;
    }

    pub fn is_closed(&self) -> bool {
        self.ended
    }

    /// Inserts `text` at the insertion point and moves the insertion point
    /// after it (`document.write`). Without an insertion point the text
    /// goes at the end of the stream.
    pub fn insert(&mut self, text: &str) {
        let mut normalized = Vec::new();
        normalize_into(text, &mut normalized, false);
        match self.insertion_point {
            Some(at) => {
                let len = normalized.len();
                let tail = self.chars.split_off(at);
                self.chars.extend(normalized);
                self.chars.extend(tail);
                self.insertion_point = Some(at + len);
            }
            None => self.chars.extend(normalized),
        }
    }

    /// Sets the insertion point just before the next input character.
    pub fn set_insertion_point_here(&mut self) {
        self.insertion_point = Some(self.pos);
    }

    /// The insertion point as an opaque value (to save and restore it).
    pub fn insertion_point(&self) -> Option<usize> {
        self.insertion_point
    }

    pub fn set_insertion_point(&mut self, point: Option<usize>) {
        self.insertion_point = point;
    }

    /// Makes the tokenizer stop at the insertion point (true) or at the end
    /// of the stream (false).
    pub fn set_stop_at_insertion_point(&mut self, stop: bool) {
        self.stop_at_insertion_point = stop;
    }

    /// True when the tokenizer has consumed everything up to the insertion
    /// point.
    pub fn at_insertion_point(&self) -> bool {
        self.insertion_point.is_some_and(|ip| self.pos >= ip)
    }

    fn limit(&self) -> usize {
        match (self.stop_at_insertion_point, self.insertion_point) {
            (true, Some(ip)) => ip.min(self.chars.len()),
            _ => self.chars.len(),
        }
    }

    /// True if no character is left before the limit and none can come.
    fn at_eof(&self) -> bool {
        self.ended && !self.stop_at_insertion_point && self.pos >= self.chars.len()
    }

    /// Consumes the next input character.
    pub fn next(&mut self) -> Next {
        if self.pos < self.limit() {
            let c = self.chars[self.pos];
            self.pos += 1;
            Next::Char(c)
        } else if self.at_eof() {
            Next::Eof
        } else {
            Next::Wait
        }
    }

    /// Puts back the character consumed last.
    pub fn unconsume(&mut self) {
        self.pos -= 1;
    }

    /// The character `offset` positions after the next one, without
    /// consuming it.
    pub fn peek(&self, offset: usize) -> Next {
        let at = self.pos + offset;
        if at < self.limit() {
            Next::Char(self.chars[at])
        } else if self.ended && !self.stop_at_insertion_point {
            Next::Eof
        } else {
            Next::Wait
        }
    }

    /// Consumes `n` characters already peeked.
    pub fn skip(&mut self, n: usize) {
        self.pos += n;
    }

    /// Whether the next characters match `word` (ASCII case-insensitively
    /// if asked): `Some(true)` or `Some(false)` once decided, `None` while
    /// more input is needed to decide.
    pub fn lookahead(&self, word: &str, ignore_case: bool) -> Option<bool> {
        for (i, w) in word.chars().enumerate() {
            match self.peek(i) {
                Next::Char(c) => {
                    let same = if ignore_case {
                        c.eq_ignore_ascii_case(&w)
                    } else {
                        c == w
                    };
                    if !same {
                        return Some(false);
                    }
                }
                Next::Eof => return Some(false),
                Next::Wait => return None,
            }
        }
        Some(true)
    }

    /// Drops consumed characters when nothing refers to them any more.
    pub fn compact(&mut self) {
        if self.insertion_point.is_none() && self.pos > 1 << 16 {
            self.chars.drain(..self.pos);
            self.pos = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(input: &mut InputStream) -> String {
        let mut out = String::new();
        while let Next::Char(c) = input.next() {
            out.push(c);
        }
        out
    }

    #[test]
    fn newlines_are_normalized_across_chunks() {
        let mut input = InputStream::new();
        input.push("a\r");
        input.push("\nb\rc\r\n");
        assert_eq!(drain(&mut input), "a\nb\nc\n");
        assert_eq!(input.next(), Next::Wait);
        input.close();
        assert_eq!(input.next(), Next::Eof);
    }

    #[test]
    fn insertion_point_takes_written_text_in_order() {
        let mut input = InputStream::new();
        input.push("ab");
        input.close();
        assert_eq!(input.next(), Next::Char('a'));
        input.set_insertion_point_here();
        input.insert("X");
        input.insert("Y");
        input.set_stop_at_insertion_point(true);
        assert_eq!(drain(&mut input), "XY");
        assert!(input.at_insertion_point());
        input.set_stop_at_insertion_point(false);
        input.set_insertion_point(None);
        assert_eq!(drain(&mut input), "b");
        assert_eq!(input.next(), Next::Eof);
    }

    #[test]
    fn lookahead_waits_for_input() {
        let mut input = InputStream::new();
        input.push("DOC");
        assert_eq!(input.lookahead("doctype", true), None);
        assert_eq!(input.lookahead("[CDATA[", false), Some(false));
        input.push("TYPE");
        assert_eq!(input.lookahead("doctype", true), Some(true));
    }
}
