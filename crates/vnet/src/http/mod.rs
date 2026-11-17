//! HTTP/1.1 (RFC 9112): message parsing and serialization, a client with
//! persistent connections and a pool of network threads, and the server
//! pieces used by the WebDriver server.
//!
//! Header names and values are kept as `String`s decoded as Latin-1, so every
//! byte on the wire maps to one `char` and back (see [`latin1_bytes`]).

mod client;
mod fetch;
mod parse;
mod server;

pub use client::{Client, Event, Response, Timeouts};
pub use fetch::{FetchId, FetchMessage, Fetcher, MAX_NETWORK_THREADS};
pub use parse::{ResponseEvent, ResponseHead, ResponseParser};
pub use server::{RequestReader, ServerRequest, ServerResponse, encode_chunk, reason_phrase};

use crate::inflate::InflateError;
use std::fmt;
use std::io;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpError {
    InvalidUrl(String),
    UnsupportedScheme(String),
    Resolve(String),
    Io(io::ErrorKind, String),
    Tls(String),
    Timeout,
    Malformed(&'static str),
    TooLarge,
    Decode(InflateError),
    ConnectionClosed,
    Cancelled,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::InvalidUrl(u) => write!(f, "invalid URL: {u}"),
            HttpError::UnsupportedScheme(s) => write!(f, "unsupported scheme: {s}"),
            HttpError::Resolve(m) => write!(f, "name resolution failed: {m}"),
            HttpError::Io(_, m) => write!(f, "I/O error: {m}"),
            HttpError::Tls(m) => write!(f, "TLS error: {m}"),
            HttpError::Timeout => write!(f, "timed out"),
            HttpError::Malformed(what) => write!(f, "malformed message: {what}"),
            HttpError::TooLarge => write!(f, "message too large"),
            HttpError::Decode(e) => write!(f, "content decoding failed: {e}"),
            HttpError::ConnectionClosed => write!(f, "connection closed"),
            HttpError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for HttpError {}

impl From<io::Error> for HttpError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => HttpError::Timeout,
            kind => HttpError::Io(kind, e.to_string()),
        }
    }
}

impl From<InflateError> for HttpError {
    fn from(e: InflateError) -> Self {
        HttpError::Decode(e)
    }
}

/// Bytes to `String`, one `char` per byte.
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Inverse of [`latin1`]; characters above U+00FF become `?`.
pub fn latin1_bytes(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
        .collect()
}

/// Ordered header list with case-insensitive lookup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers {
    list: Vec<(String, String)>,
}

impl Headers {
    pub fn new() -> Headers {
        Headers::default()
    }

    pub fn add(&mut self, name: &str, value: &str) {
        self.list.push((name.to_string(), value.to_string()));
    }

    /// Replace every header named `name` with a single one.
    pub fn set(&mut self, name: &str, value: &str) {
        self.remove(name);
        self.add(name, value);
    }

    pub fn remove(&mut self, name: &str) {
        self.list.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.list
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.list
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Comma-separated list elements of every header named `name`, trimmed.
    pub fn tokens(&self, name: &str) -> Vec<String> {
        self.get_all(name)
            .flat_map(|v| v.split(','))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// Whether the list header `name` has `token` (case-insensitive).
    pub fn has_token(&self, name: &str, token: &str) -> bool {
        self.tokens(name)
            .iter()
            .any(|t| t.eq_ignore_ascii_case(token))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.list.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        for (n, v) in &self.list {
            out.extend(latin1_bytes(n));
            out.extend_from_slice(b": ");
            out.extend(latin1_bytes(v));
            out.extend_from_slice(b"\r\n");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    pub fn default_port(self) -> u16 {
        match self {
            Scheme::Http => 80,
            Scheme::Https => 443,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }
}

/// Connection pool key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
}

/// Absolute `http`/`https` URL split for the wire. Full URL parsing belongs to
/// the browser; this accepts the serialized URLs it produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub scheme: Scheme,
    /// Lowercase host; IPv6 addresses without brackets.
    pub host: String,
    pub port: u16,
    /// Path and query (request-target in origin-form), never empty.
    pub target: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, HttpError> {
        let bad = || HttpError::InvalidUrl(s.to_string());
        let (scheme, rest) = s.split_once("://").ok_or_else(bad)?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            other => return Err(HttpError::UnsupportedScheme(other.to_string())),
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        let mut target = rest[end..].split('#').next().unwrap_or("").to_string();
        if !target.starts_with('/') {
            target.insert(0, '/');
        }
        let hostport = match authority.rfind('@') {
            Some(i) => &authority[i + 1..],
            None => authority,
        };
        let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
            let close = v6.find(']').ok_or_else(bad)?;
            let host = &v6[..close];
            let after = &v6[close + 1..];
            let port = match after.strip_prefix(':') {
                Some(p) => Some(p),
                None if after.is_empty() => None,
                None => return Err(bad()),
            };
            (host, port)
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (hostport, None),
            }
        };
        if host.is_empty() {
            return Err(bad());
        }
        let port = match port {
            None | Some("") => scheme.default_port(),
            Some(p) => p.parse::<u16>().map_err(|_| bad())?,
        };
        Ok(Url {
            scheme,
            host: host.to_ascii_lowercase(),
            port,
            target,
        })
    }

    pub fn origin(&self) -> Origin {
        Origin {
            scheme: self.scheme,
            host: self.host.clone(),
            port: self.port,
        }
    }

    /// Value of the `Host` header.
    pub fn host_header(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == self.scheme.default_port() {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

/// Outgoing client request.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub url: Url,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: &str, url: Url) -> Request {
        Request {
            method: method.to_string(),
            url,
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    pub fn get(url: &str) -> Result<Request, HttpError> {
        Ok(Request::new("GET", Url::parse(url)?))
    }

    /// Safe to resend after a stale persistent connection (RFC 9110 §9.2.2).
    pub fn is_idempotent(&self) -> bool {
        matches!(
            self.method.as_str(),
            "GET" | "HEAD" | "PUT" | "DELETE" | "OPTIONS" | "TRACE"
        )
    }

    /// Serialized request; adds `Host`, `Accept-Encoding`, `User-Agent` and
    /// `Content-Length` unless already present.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256 + self.body.len());
        out.extend(latin1_bytes(&self.method));
        out.push(b' ');
        out.extend(latin1_bytes(&self.url.target));
        out.extend_from_slice(b" HTTP/1.1\r\n");
        let mut extra = Headers::new();
        if !self.headers.contains("host") {
            extra.add("Host", &self.url.host_header());
        }
        if !self.headers.contains("user-agent") {
            extra.add("User-Agent", "vidrio/0.1");
        }
        if !self.headers.contains("accept-encoding") {
            extra.add("Accept-Encoding", "gzip, deflate");
        }
        let has_body = !self.body.is_empty() || matches!(self.method.as_str(), "POST" | "PUT");
        if has_body
            && !self.headers.contains("content-length")
            && !self.headers.contains("transfer-encoding")
        {
            extra.add("Content-Length", &self.body.len().to_string());
        }
        extra.write_to(&mut out);
        self.headers.write_to(&mut out);
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_split() {
        let u = Url::parse("http://User@Example.TEST:8000/a/b?q=1#frag").unwrap();
        assert_eq!(u.host, "example.test");
        assert_eq!(u.port, 8000);
        assert_eq!(u.target, "/a/b?q=1");
        assert_eq!(u.host_header(), "example.test:8000");
        let v6 = Url::parse("https://[::1]?x").unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("::1", 443));
        assert_eq!(v6.target, "/?x");
        assert_eq!(v6.host_header(), "[::1]");
        assert!(Url::parse("ftp://x/").is_err());
        assert!(Url::parse("http://:80/").is_err());
    }

    #[test]
    fn request_serialization() {
        let mut r = Request::new("POST", Url::parse("http://a.test/x").unwrap());
        r.headers.add("X-Test", "1");
        r.body = b"hi".to_vec();
        let s = String::from_utf8(r.to_bytes()).unwrap();
        assert!(s.starts_with("POST /x HTTP/1.1\r\nHost: a.test\r\n"));
        assert!(s.contains("\r\nContent-Length: 2\r\n"));
        assert!(s.ends_with("X-Test: 1\r\n\r\nhi"));
    }
}
