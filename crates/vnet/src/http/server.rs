//! Server side of HTTP/1.1: reading requests and writing responses.

use super::parse::{
    Chunked, Framing, MAX_HEAD, content_length, find_head_end, parse_head, parse_version,
};
use super::{Headers, HttpError};
use std::io::{self, Read, Write};

/// Largest request body accepted.
const MAX_BODY: usize = 64 * 1024 * 1024;

/// A complete request received by a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerRequest {
    pub method: String,
    /// Request-target as sent (origin-form for ordinary requests).
    pub target: String,
    pub version: (u8, u8),
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl ServerRequest {
    /// Whether the client allows another request on this connection.
    pub fn keep_alive(&self) -> bool {
        if self.version >= (1, 1) {
            !self.headers.has_token("connection", "close")
        } else {
            self.headers.has_token("connection", "keep-alive")
        }
    }

    /// Path of the target without the query.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }
}

/// Reads successive requests from one connection, keeping bytes that belong
/// to the next request.
pub struct RequestReader<R> {
    inner: R,
    buf: Vec<u8>,
}

impl<R: Read> RequestReader<R> {
    pub fn new(inner: R) -> RequestReader<R> {
        RequestReader {
            inner,
            buf: Vec::new(),
        }
    }

    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    fn fill(&mut self) -> Result<usize, HttpError> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match self.inner.read(&mut chunk) {
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Next request, or `None` if the peer closed the connection between requests.
    pub fn read_request(&mut self) -> Result<Option<ServerRequest>, HttpError> {
        // Empty lines before a request line are ignored (RFC 9112 §2.2).
        let end = loop {
            let skip = self
                .buf
                .iter()
                .position(|&b| b != b'\r' && b != b'\n')
                .unwrap_or(self.buf.len());
            self.buf.drain(..skip);
            if let Some(end) = find_head_end(&self.buf, 0) {
                break end;
            }
            if self.buf.len() > MAX_HEAD {
                return Err(HttpError::TooLarge);
            }
            if self.fill()? == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Err(HttpError::ConnectionClosed);
            }
        };
        let (start, headers) = parse_head(&self.buf[..end])?;
        self.buf.drain(..end);
        let mut parts = start.split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(HttpError::Malformed("request line"));
        };
        if method.is_empty() || target.is_empty() {
            return Err(HttpError::Malformed("request line"));
        }
        let version = parse_version(version)?;
        let mut framing = if headers.contains("transfer-encoding") {
            let codings = headers.tokens("transfer-encoding");
            if codings.len() == 1 && codings[0].eq_ignore_ascii_case("chunked") {
                Framing::Chunked(Chunked::new())
            } else {
                return Err(HttpError::Malformed("unsupported Transfer-Encoding"));
            }
        } else {
            match content_length(&headers)? {
                Some(n) if n > MAX_BODY as u64 => return Err(HttpError::TooLarge),
                Some(0) | None => Framing::Empty,
                Some(n) => Framing::Length(n),
            }
        };
        let mut body = Vec::new();
        loop {
            let (used, done) = framing.decode(&self.buf, &mut body)?;
            self.buf.drain(..used);
            if done {
                break;
            }
            if body.len() > MAX_BODY {
                return Err(HttpError::TooLarge);
            }
            if self.fill()? == 0 {
                return Err(HttpError::ConnectionClosed);
            }
        }
        Ok(Some(ServerRequest {
            method: method.to_string(),
            target: target.to_string(),
            version,
            headers,
            body,
        }))
    }
}

/// Response written by a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl ServerResponse {
    pub fn new(status: u16) -> ServerResponse {
        ServerResponse {
            status,
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> ServerResponse {
        self.headers.add(name, value);
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> ServerResponse {
        self.body = body.into();
        self
    }

    fn bodyless(&self, head_request: bool) -> bool {
        head_request
            || (100..200).contains(&self.status)
            || self.status == 204
            || self.status == 304
    }

    /// Status line and fields, ending with the blank line. Adds
    /// `Content-Length` unless the response is chunked or has no body.
    pub fn head_bytes(&self) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {} {}\r\n",
            self.status,
            reason_phrase(self.status)
        )
        .into_bytes();
        let chunked = self.headers.has_token("transfer-encoding", "chunked");
        let no_length = (100..200).contains(&self.status) || self.status == 204;
        if !chunked && !no_length && !self.headers.contains("content-length") {
            out.extend_from_slice(format!("Content-Length: {}\r\n", self.body.len()).as_bytes());
        }
        self.headers.write_to(&mut out);
        out.extend_from_slice(b"\r\n");
        out
    }

    /// Whole response. A response with `Transfer-Encoding: chunked` sends its
    /// body as one chunk followed by the last chunk.
    pub fn to_bytes(&self, head_request: bool) -> Vec<u8> {
        let mut out = self.head_bytes();
        if self.bodyless(head_request) {
            return out;
        }
        if self.headers.has_token("transfer-encoding", "chunked") {
            if !self.body.is_empty() {
                out.extend(encode_chunk(&self.body));
            }
            out.extend(encode_chunk(&[]));
        } else {
            out.extend_from_slice(&self.body);
        }
        out
    }

    pub fn write_to<W: Write>(&self, w: &mut W, head_request: bool) -> io::Result<()> {
        w.write_all(&self.to_bytes(head_request))?;
        w.flush()
    }
}

/// One chunk of the `chunked` coding; an empty `data` gives the last chunk
/// (with the final blank line).
pub fn encode_chunk(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:x}\r\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    out
}

pub fn reason_phrase(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pipelined_requests() {
        let wire = b"\r\nPOST /session HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello\
GET /status HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        let mut r = RequestReader::new(&wire[..]);
        let a = r.read_request().unwrap().unwrap();
        assert_eq!(
            (a.method.as_str(), a.path(), &a.body[..]),
            ("POST", "/session", &b"hello"[..])
        );
        let b = r.read_request().unwrap().unwrap();
        assert_eq!((b.method.as_str(), &b.body[..]), ("GET", &b"abc"[..]));
        assert!(b.keep_alive());
        assert_eq!(r.read_request().unwrap(), None);
    }

    #[test]
    fn writes_response() {
        let r = ServerResponse::new(404)
            .header("Content-Type", "application/json")
            .body("{}");
        assert_eq!(
            r.to_bytes(false),
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nContent-Type: application/json\r\n\r\n{}"
        );
        assert!(r.to_bytes(true).ends_with(b"\r\n\r\n"));
    }
}
