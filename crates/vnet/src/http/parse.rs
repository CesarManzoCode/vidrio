//! Incremental parsing of HTTP/1.1 message heads and bodies (RFC 9112).

use super::{Headers, HttpError, latin1};

/// Upper bound for a message head (start line and fields).
pub(crate) const MAX_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 256;
const MAX_TRAILERS: usize = 64 * 1024;

/// Index just past the blank line ending a head, searching from `from`.
pub(crate) fn find_head_end(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < buf.len() {
        if buf[i] == b'\n' {
            match (buf.get(i + 1), buf.get(i + 2)) {
                (Some(b'\n'), _) => return Some(i + 2),
                (Some(b'\r'), Some(b'\n')) => return Some(i + 3),
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn is_tchar(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
}

/// Split a head into its start line and header fields.
pub(crate) fn parse_head(head: &[u8]) -> Result<(String, Headers), HttpError> {
    let text = latin1(head);
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let start = lines.next().unwrap_or("").to_string();
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            // Obsolete line folding: continue the previous value.
            let Some(last) = fields.last_mut() else {
                return Err(HttpError::Malformed("folded line without field"));
            };
            last.1.push(' ');
            last.1.push_str(line.trim_matches([' ', '\t']));
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpError::Malformed("field without colon"));
        };
        if name.is_empty() || !name.chars().all(is_tchar) {
            return Err(HttpError::Malformed("invalid field name"));
        }
        fields.push((
            name.to_string(),
            value.trim_matches([' ', '\t']).to_string(),
        ));
        if fields.len() > MAX_HEADERS {
            return Err(HttpError::TooLarge);
        }
    }
    let mut headers = Headers::new();
    for (n, v) in fields {
        headers.add(&n, &v);
    }
    Ok((start, headers))
}

/// `HTTP/x.y` to `(x, y)`.
pub(crate) fn parse_version(s: &str) -> Result<(u8, u8), HttpError> {
    let v = s
        .strip_prefix("HTTP/")
        .ok_or(HttpError::Malformed("version"))?;
    let b = v.as_bytes();
    if b.len() != 3 || !b[0].is_ascii_digit() || b[1] != b'.' || !b[2].is_ascii_digit() {
        return Err(HttpError::Malformed("version"));
    }
    Ok((b[0] - b'0', b[2] - b'0'))
}

/// Content-Length: every value (and list element) must be the same number.
pub(crate) fn content_length(headers: &Headers) -> Result<Option<u64>, HttpError> {
    let mut found: Option<u64> = None;
    for t in headers.tokens("content-length") {
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
            return Err(HttpError::Malformed("Content-Length"));
        }
        let n: u64 = t
            .parse()
            .map_err(|_| HttpError::Malformed("Content-Length"))?;
        if found.is_some_and(|f| f != n) {
            return Err(HttpError::Malformed("conflicting Content-Length"));
        }
        found = Some(n);
    }
    if found.is_none() && headers.contains("content-length") {
        return Err(HttpError::Malformed("Content-Length"));
    }
    Ok(found)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChunkState {
    Size,
    Extension,
    SizeLf,
    Data,
    DataCr,
    DataLf,
    Trailer,
    Done,
}

/// `chunked` transfer coding decoder, resumable at any byte.
#[derive(Clone, Debug)]
pub(crate) struct Chunked {
    state: ChunkState,
    size: u64,
    digits: usize,
    line: Vec<u8>,
    trailer_bytes: usize,
}

impl Chunked {
    pub(crate) fn new() -> Chunked {
        Chunked {
            state: ChunkState::Size,
            size: 0,
            digits: 0,
            line: Vec::new(),
            trailer_bytes: 0,
        }
    }

    fn after_size(&mut self) -> ChunkState {
        if self.size == 0 {
            ChunkState::Trailer
        } else {
            ChunkState::Data
        }
    }

    fn decode(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(usize, bool), HttpError> {
        let mut i = 0;
        while i < input.len() {
            let b = input[i];
            match self.state {
                ChunkState::Size => {
                    if let Some(v) = (b as char).to_digit(16) {
                        if self.size > (u64::MAX >> 8) {
                            return Err(HttpError::Malformed("chunk size overflow"));
                        }
                        self.size = self.size * 16 + v as u64;
                        self.digits += 1;
                    } else if self.digits == 0 {
                        return Err(HttpError::Malformed("chunk size"));
                    } else if b == b';' || b == b' ' || b == b'\t' {
                        self.state = ChunkState::Extension;
                    } else if b == b'\r' {
                        self.state = ChunkState::SizeLf;
                    } else if b == b'\n' {
                        self.state = self.after_size();
                    } else {
                        return Err(HttpError::Malformed("chunk size"));
                    }
                    i += 1;
                }
                ChunkState::Extension => {
                    if b == b'\n' {
                        self.state = self.after_size();
                    }
                    i += 1;
                }
                ChunkState::SizeLf => {
                    if b != b'\n' {
                        return Err(HttpError::Malformed("chunk size line"));
                    }
                    self.state = self.after_size();
                    i += 1;
                }
                ChunkState::Data => {
                    let n = (self.size.min((input.len() - i) as u64)) as usize;
                    out.extend_from_slice(&input[i..i + n]);
                    self.size -= n as u64;
                    i += n;
                    if self.size == 0 {
                        self.state = ChunkState::DataCr;
                    }
                }
                ChunkState::DataCr => {
                    self.state = match b {
                        b'\r' => ChunkState::DataLf,
                        b'\n' => {
                            self.digits = 0;
                            ChunkState::Size
                        }
                        _ => return Err(HttpError::Malformed("chunk data terminator")),
                    };
                    i += 1;
                }
                ChunkState::DataLf => {
                    if b != b'\n' {
                        return Err(HttpError::Malformed("chunk data terminator"));
                    }
                    self.digits = 0;
                    self.state = ChunkState::Size;
                    i += 1;
                }
                ChunkState::Trailer => {
                    i += 1;
                    self.trailer_bytes += 1;
                    if self.trailer_bytes > MAX_TRAILERS {
                        return Err(HttpError::TooLarge);
                    }
                    if b != b'\n' {
                        self.line.push(b);
                        continue;
                    }
                    let blank = self.line.is_empty() || self.line == b"\r";
                    self.line.clear();
                    if blank {
                        self.state = ChunkState::Done;
                        return Ok((i, true));
                    }
                    // Trailer fields are read and dropped.
                }
                ChunkState::Done => return Ok((i, true)),
            }
        }
        Ok((i, self.state == ChunkState::Done))
    }
}

/// How the end of a message body is found.
#[derive(Clone, Debug)]
pub(crate) enum Framing {
    Empty,
    Length(u64),
    Chunked(Chunked),
    /// Until the connection closes (responses only).
    Close,
}

impl Framing {
    /// Consume body bytes from `input` into `out`; returns bytes consumed and
    /// whether the body is complete. Bytes past the end are not consumed.
    pub(crate) fn decode(
        &mut self,
        input: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(usize, bool), HttpError> {
        match self {
            Framing::Empty => Ok((0, true)),
            Framing::Length(rem) => {
                let n = (*rem).min(input.len() as u64) as usize;
                out.extend_from_slice(&input[..n]);
                *rem -= n as u64;
                Ok((n, *rem == 0))
            }
            Framing::Chunked(c) => c.decode(input, out),
            Framing::Close => {
                out.extend_from_slice(input);
                Ok((input.len(), false))
            }
        }
    }
}

/// Status line and fields of a response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseHead {
    pub version: (u8, u8),
    pub status: u16,
    pub reason: String,
    pub headers: Headers,
    /// Set by the client when it removed the `Content-Encoding` (the header
    /// itself is kept).
    pub content_decoded: bool,
}

impl ResponseHead {
    fn parse(head: &[u8]) -> Result<ResponseHead, HttpError> {
        let (start, headers) = parse_head(head)?;
        let mut parts = start.splitn(3, ' ');
        let version = parse_version(parts.next().unwrap_or(""))?;
        let code = parts.next().unwrap_or("");
        if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(HttpError::Malformed("status code"));
        }
        let status: u16 = code
            .parse()
            .map_err(|_| HttpError::Malformed("status code"))?;
        Ok(ResponseHead {
            version,
            status,
            reason: parts.next().unwrap_or("").to_string(),
            headers,
            content_decoded: false,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponseEvent {
    Head(ResponseHead),
    Data(Vec<u8>),
    End,
}

enum ParseState {
    Head,
    Body(Framing),
    Done,
}

/// Push parser for one response; bytes may arrive split anywhere.
pub struct ResponseParser {
    head_request: bool,
    buf: Vec<u8>,
    state: ParseState,
    keep_alive: bool,
}

impl ResponseParser {
    /// `head_request`: the request was `HEAD`, so the response has no body.
    pub fn new(head_request: bool) -> ResponseParser {
        ResponseParser {
            head_request,
            buf: Vec::new(),
            state: ParseState::Head,
            keep_alive: false,
        }
    }

    pub fn is_done(&self) -> bool {
        matches!(self.state, ParseState::Done)
    }

    /// Whether the connection may carry another request after this response.
    pub fn keep_alive(&self) -> bool {
        self.keep_alive
    }

    /// Feed bytes; returns how many were consumed. Parsing stops after the
    /// end of the response, leaving later bytes unconsumed. Interim (1xx)
    /// responses other than 101 are skipped.
    pub fn feed(
        &mut self,
        input: &[u8],
        events: &mut Vec<ResponseEvent>,
    ) -> Result<usize, HttpError> {
        let mut pos = 0;
        loop {
            match &mut self.state {
                ParseState::Done => return Ok(pos),
                ParseState::Head => {
                    if pos == input.len() {
                        return Ok(pos);
                    }
                    let old = self.buf.len();
                    self.buf.extend_from_slice(&input[pos..]);
                    let Some(end) = find_head_end(&self.buf, old.saturating_sub(2)) else {
                        if self.buf.len() > MAX_HEAD {
                            return Err(HttpError::TooLarge);
                        }
                        return Ok(input.len());
                    };
                    pos += end - old;
                    let head = ResponseHead::parse(&self.buf[..end])?;
                    self.buf.clear();
                    if (100..200).contains(&head.status) && head.status != 101 {
                        continue;
                    }
                    let framing = self.framing(&head)?;
                    events.push(ResponseEvent::Head(head));
                    self.state = ParseState::Body(framing);
                }
                ParseState::Body(framing) => {
                    let mut out = Vec::new();
                    let (n, done) = framing.decode(&input[pos..], &mut out)?;
                    pos += n;
                    if !out.is_empty() {
                        events.push(ResponseEvent::Data(out));
                    }
                    if !done {
                        return Ok(pos);
                    }
                    self.state = ParseState::Done;
                    events.push(ResponseEvent::End);
                }
            }
        }
    }

    /// The connection was closed by the peer.
    pub fn feed_eof(&mut self, events: &mut Vec<ResponseEvent>) -> Result<(), HttpError> {
        match &self.state {
            ParseState::Done => Ok(()),
            ParseState::Body(Framing::Close) => {
                self.state = ParseState::Done;
                self.keep_alive = false;
                events.push(ResponseEvent::End);
                Ok(())
            }
            _ => Err(HttpError::ConnectionClosed),
        }
    }

    /// Body framing per RFC 9112 §6.3; also decides persistence.
    fn framing(&mut self, head: &ResponseHead) -> Result<Framing, HttpError> {
        let h = &head.headers;
        let persistent = if head.version >= (1, 1) {
            !h.has_token("connection", "close")
        } else {
            h.has_token("connection", "keep-alive")
        };
        let framing = if self.head_request
            || (100..200).contains(&head.status)
            || head.status == 204
            || head.status == 304
        {
            Framing::Empty
        } else if h.contains("transfer-encoding") {
            let codings = h.tokens("transfer-encoding");
            if codings
                .last()
                .is_some_and(|c| c.eq_ignore_ascii_case("chunked"))
            {
                Framing::Chunked(Chunked::new())
            } else {
                Framing::Close
            }
        } else {
            match content_length(h)? {
                Some(0) => Framing::Empty,
                Some(n) => Framing::Length(n),
                None => Framing::Close,
            }
        };
        self.keep_alive = persistent && !matches!(framing, Framing::Close) && head.status != 101;
        Ok(framing)
    }
}
