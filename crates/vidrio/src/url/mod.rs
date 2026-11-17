//! WHATWG URL: the URL record, the basic URL parser with its whole state
//! machine (state override included, for the setters of the URL API), the
//! URL serializer, origins, hosts (with UTS #46 and Punycode),
//! percent-encoding and `application/x-www-form-urlencoded`.

pub mod form;
pub mod host;
pub mod idna;
pub mod percent;

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

pub use host::{Host, domain_to_ascii, domain_to_unicode, parse_host};
use percent::{PercentEncodeSet, percent_encode_after_encoding, utf8_percent_encode_char};

use crate::encoding::Encoding;

/// The path of a URL: opaque (a URL like `mailto:x`) or a list of
/// segments.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum UrlPath {
    Opaque(String),
    Segments(Vec<String>),
}

/// A URL record (URL §4.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Url {
    pub scheme: String,
    pub username: String,
    pub password: String,
    pub host: Option<Host>,
    pub port: Option<u16>,
    pub path: UrlPath,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

/// The basic URL parser returned failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError;

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid URL")
    }
}

impl std::error::Error for ParseError {}

/// The states of the basic URL parser (URL §4.4); the ones a setter passes
/// as state override are public.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    SchemeStart,
    Scheme,
    NoScheme,
    SpecialRelativeOrAuthority,
    PathOrAuthority,
    Relative,
    RelativeSlash,
    SpecialAuthoritySlashes,
    SpecialAuthorityIgnoreSlashes,
    Authority,
    Host,
    Hostname,
    Port,
    File,
    FileSlash,
    FileHost,
    PathStart,
    Path,
    OpaquePath,
    Query,
    Fragment,
}

/// The default port of a special scheme; `None` for `file` and for
/// non-special schemes.
pub fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "ftp" => Some(21),
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

/// Whether `scheme` is a special scheme (URL §4.1).
pub fn is_special_scheme(scheme: &str) -> bool {
    matches!(scheme, "ftp" | "file" | "http" | "https" | "ws" | "wss")
}

fn is_windows_drive_letter(s: &[char]) -> bool {
    s.len() == 2 && s[0].is_ascii_alphabetic() && (s[1] == ':' || s[1] == '|')
}

fn is_normalized_windows_drive_letter(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn starts_with_windows_drive_letter(s: &[char]) -> bool {
    s.len() >= 2
        && is_windows_drive_letter(&s[..2])
        && (s.len() == 2 || matches!(s[2], '/' | '\\' | '?' | '#'))
}

fn is_single_dot_segment(s: &str) -> bool {
    s == "." || s.eq_ignore_ascii_case("%2e")
}

fn is_double_dot_segment(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        ".." | ".%2e" | "%2e." | "%2e%2e"
    )
}

impl Url {
    /// Parse `input` against an optional `base` (the URL parser, URL §4.4).
    pub fn parse(input: &str, base: Option<&Url>) -> Result<Url, ParseError> {
        Url::parse_with_encoding(input, base, Encoding::Utf8)
    }

    /// Parse with the query encoded in `encoding` (a document's encoding).
    pub fn parse_with_encoding(
        input: &str,
        base: Option<&Url>,
        encoding: Encoding,
    ) -> Result<Url, ParseError> {
        let mut url = Url {
            scheme: String::new(),
            username: String::new(),
            password: String::new(),
            host: None,
            port: None,
            path: UrlPath::Segments(Vec::new()),
            query: None,
            fragment: None,
        };
        let trimmed = input.trim_matches(|c: char| c <= ' ');
        basic_parse(trimmed, base, encoding.output_encoding(), &mut url, None)?;
        Ok(url)
    }

    /// Whether the scheme is special.
    pub fn is_special(&self) -> bool {
        is_special_scheme(&self.scheme)
    }

    /// Whether the URL has a non-empty username or password.
    pub fn includes_credentials(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }

    /// Whether the path is opaque.
    pub fn has_opaque_path(&self) -> bool {
        matches!(self.path, UrlPath::Opaque(_))
    }

    /// Whether the URL cannot have a username, password or port.
    pub fn cannot_have_username_password_port(&self) -> bool {
        matches!(self.host, None | Some(Host::Empty)) || self.scheme == "file"
    }

    /// Shorten a URL's path (URL §4.1).
    fn shorten_path(&mut self) {
        let is_file = self.scheme == "file";
        if let UrlPath::Segments(segments) = &mut self.path {
            if is_file && segments.len() == 1 && is_normalized_windows_drive_letter(&segments[0]) {
                return;
            }
            segments.pop();
        }
    }

    fn segments_mut(&mut self) -> &mut Vec<String> {
        if let UrlPath::Opaque(_) = self.path {
            self.path = UrlPath::Segments(Vec::new());
        }
        match &mut self.path {
            UrlPath::Segments(segments) => segments,
            UrlPath::Opaque(_) => unreachable!(),
        }
    }

    /// The URL path serializer (URL §4.5).
    pub fn path_string(&self) -> String {
        match &self.path {
            UrlPath::Opaque(path) => path.clone(),
            UrlPath::Segments(segments) => {
                let mut out = String::new();
                for segment in segments {
                    out.push('/');
                    out.push_str(segment);
                }
                out
            }
        }
    }

    /// The URL serializer (URL §4.5).
    pub fn serialize(&self, exclude_fragment: bool) -> String {
        let mut output = format!("{}:", self.scheme);
        if let Some(host) = &self.host {
            output.push_str("//");
            if self.includes_credentials() {
                output.push_str(&self.username);
                if !self.password.is_empty() {
                    output.push(':');
                    output.push_str(&self.password);
                }
                output.push('@');
            }
            output.push_str(&host.to_string());
            if let Some(port) = self.port {
                output.push_str(&format!(":{port}"));
            }
        }
        if let UrlPath::Segments(segments) = &self.path {
            if self.host.is_none() && segments.len() > 1 && segments[0].is_empty() {
                output.push_str("/.");
            }
        }
        output.push_str(&self.path_string());
        if let Some(query) = &self.query {
            output.push('?');
            output.push_str(query);
        }
        if !exclude_fragment {
            if let Some(fragment) = &self.fragment {
                output.push('#');
                output.push_str(fragment);
            }
        }
        output
    }

    /// The href: the URL serialized with its fragment.
    pub fn href(&self) -> String {
        self.serialize(false)
    }

    /// The origin of the URL (URL §4.7). `blob:` URLs take the origin of
    /// the URL in their path when it is HTTP(S); the blob URL store is not
    /// consulted here.
    pub fn origin(&self) -> Origin {
        match self.scheme.as_str() {
            "blob" => {
                let path = self.path_string();
                match Url::parse(&path, None) {
                    Ok(inner) if inner.scheme == "http" || inner.scheme == "https" => {
                        inner.origin()
                    }
                    _ => Origin::new_opaque(),
                }
            }
            "ftp" | "http" | "https" | "ws" | "wss" => Origin::Tuple {
                scheme: self.scheme.clone(),
                host: self.host.clone().unwrap_or(Host::Empty),
                port: self.port,
                domain: None,
            },
            _ => Origin::new_opaque(),
        }
    }

    // ----- setters of the URL API (URL §6.1) -----

    /// The `protocol` setter.
    pub fn set_protocol(&mut self, value: &str) {
        let input = format!("{value}:");
        let _ = basic_parse(&input, None, Encoding::Utf8, self, Some(State::SchemeStart));
    }

    /// The `username` setter.
    pub fn set_username(&mut self, value: &str) {
        if self.cannot_have_username_password_port() {
            return;
        }
        self.username = percent::utf8_percent_encode(value, PercentEncodeSet::Userinfo);
    }

    /// The `password` setter.
    pub fn set_password(&mut self, value: &str) {
        if self.cannot_have_username_password_port() {
            return;
        }
        self.password = percent::utf8_percent_encode(value, PercentEncodeSet::Userinfo);
    }

    /// The `host` setter.
    pub fn set_host(&mut self, value: &str) {
        if self.has_opaque_path() {
            return;
        }
        let _ = basic_parse(value, None, Encoding::Utf8, self, Some(State::Host));
    }

    /// The `hostname` setter.
    pub fn set_hostname(&mut self, value: &str) {
        if self.has_opaque_path() {
            return;
        }
        let _ = basic_parse(value, None, Encoding::Utf8, self, Some(State::Hostname));
    }

    /// The `port` setter.
    pub fn set_port(&mut self, value: &str) {
        if self.cannot_have_username_password_port() {
            return;
        }
        if value.is_empty() {
            self.port = None;
        } else {
            let _ = basic_parse(value, None, Encoding::Utf8, self, Some(State::Port));
        }
    }

    /// The `pathname` setter.
    pub fn set_pathname(&mut self, value: &str) {
        if self.has_opaque_path() {
            return;
        }
        self.path = UrlPath::Segments(Vec::new());
        let _ = basic_parse(value, None, Encoding::Utf8, self, Some(State::PathStart));
    }

    /// The `search` setter.
    pub fn set_search(&mut self, value: &str) {
        if value.is_empty() {
            self.query = None;
            return;
        }
        let input = value.strip_prefix('?').unwrap_or(value);
        self.query = Some(String::new());
        let _ = basic_parse(input, None, Encoding::Utf8, self, Some(State::Query));
    }

    /// The `hash` setter.
    pub fn set_hash(&mut self, value: &str) {
        if value.is_empty() {
            self.fragment = None;
            return;
        }
        let input = value.strip_prefix('#').unwrap_or(value);
        self.fragment = Some(String::new());
        let _ = basic_parse(input, None, Encoding::Utf8, self, Some(State::Fragment));
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.href())
    }
}

/// An origin (HTML §7.1.1): opaque, or a scheme/host/port tuple with an
/// optional domain.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// An opaque origin; the number makes each one unique.
    Opaque(u64),
    Tuple {
        scheme: String,
        host: Host,
        port: Option<u16>,
        domain: Option<String>,
    },
}

impl Origin {
    /// A new opaque origin, different from every other.
    pub fn new_opaque() -> Origin {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Origin::Opaque(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Whether the origin is opaque.
    pub fn is_opaque(&self) -> bool {
        matches!(self, Origin::Opaque(_))
    }

    /// The serialization of an origin (HTML §7.1.1): `null` when opaque.
    pub fn serialize(&self) -> String {
        match self {
            Origin::Opaque(_) => "null".to_string(),
            Origin::Tuple {
                scheme, host, port, ..
            } => match port {
                Some(port) => format!("{scheme}://{host}:{port}"),
                None => format!("{scheme}://{host}"),
            },
        }
    }

    /// Same origin (HTML §7.1.1.1).
    pub fn same_origin(&self, other: &Origin) -> bool {
        match (self, other) {
            (Origin::Opaque(a), Origin::Opaque(b)) => a == b,
            (
                Origin::Tuple {
                    scheme: s1,
                    host: h1,
                    port: p1,
                    ..
                },
                Origin::Tuple {
                    scheme: s2,
                    host: h2,
                    port: p2,
                    ..
                },
            ) => s1 == s2 && h1 == h2 && p1 == p2,
            _ => false,
        }
    }
}

/// The basic URL parser (URL §4.4) on `input` already stripped of leading
/// and trailing C0 control or space when `url` is new. With
/// `state_override`, `url` is modified in place; `Err` is the parser's
/// failure, which may come after some components were already set, as the
/// standard says.
pub fn basic_parse(
    input: &str,
    base: Option<&Url>,
    mut encoding: Encoding,
    url: &mut Url,
    state_override: Option<State>,
) -> Result<(), ParseError> {
    let input: Vec<char> = input
        .chars()
        .filter(|&c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let len = input.len() as isize;
    let mut state = state_override.unwrap_or(State::SchemeStart);
    let mut buffer = String::new();
    let mut at_sign_seen = false;
    let mut inside_brackets = false;
    let mut password_token_seen = false;
    let mut pointer: isize = 0;
    let at = |p: isize| -> Option<char> {
        if p >= 0 && p < len {
            Some(input[p as usize])
        } else {
            None
        }
    };

    loop {
        let c = at(pointer);
        match state {
            State::SchemeStart => {
                if let Some(c) = c.filter(char::is_ascii_alphabetic) {
                    buffer.push(c.to_ascii_lowercase());
                    state = State::Scheme;
                } else if state_override.is_none() {
                    state = State::NoScheme;
                    pointer -= 1;
                } else {
                    return Err(ParseError);
                }
            }
            State::Scheme => {
                if let Some(c) =
                    c.filter(|c| c.is_ascii_alphanumeric() || matches!(*c, '+' | '-' | '.'))
                {
                    buffer.push(c.to_ascii_lowercase());
                } else if c == Some(':') {
                    if state_override.is_some() {
                        if url.is_special() != is_special_scheme(&buffer) {
                            return Ok(());
                        }
                        if (url.includes_credentials() || url.port.is_some()) && buffer == "file" {
                            return Ok(());
                        }
                        if url.scheme == "file" && url.host == Some(Host::Empty) {
                            return Ok(());
                        }
                    }
                    url.scheme = std::mem::take(&mut buffer);
                    if state_override.is_some() {
                        if url.port.is_some() && url.port == default_port(&url.scheme) {
                            url.port = None;
                        }
                        return Ok(());
                    }
                    if url.scheme == "file" {
                        state = State::File;
                    } else if url.is_special() && base.is_some_and(|b| b.scheme == url.scheme) {
                        state = State::SpecialRelativeOrAuthority;
                    } else if url.is_special() {
                        state = State::SpecialAuthoritySlashes;
                    } else if at(pointer + 1) == Some('/') {
                        state = State::PathOrAuthority;
                        pointer += 1;
                    } else {
                        url.path = UrlPath::Opaque(String::new());
                        state = State::OpaquePath;
                    }
                } else if state_override.is_none() {
                    buffer.clear();
                    state = State::NoScheme;
                    pointer = -1;
                } else {
                    return Err(ParseError);
                }
            }
            State::NoScheme => {
                let Some(base) = base else {
                    return Err(ParseError);
                };
                if base.has_opaque_path() && c != Some('#') {
                    return Err(ParseError);
                } else if base.has_opaque_path() {
                    url.scheme = base.scheme.clone();
                    url.path = base.path.clone();
                    url.query = base.query.clone();
                    url.fragment = Some(String::new());
                    state = State::Fragment;
                } else if base.scheme != "file" {
                    state = State::Relative;
                    pointer -= 1;
                } else {
                    state = State::File;
                    pointer -= 1;
                }
            }
            State::SpecialRelativeOrAuthority => {
                if c == Some('/') && at(pointer + 1) == Some('/') {
                    state = State::SpecialAuthorityIgnoreSlashes;
                    pointer += 1;
                } else {
                    state = State::Relative;
                    pointer -= 1;
                }
            }
            State::PathOrAuthority => {
                if c == Some('/') {
                    state = State::Authority;
                } else {
                    state = State::Path;
                    pointer -= 1;
                }
            }
            State::Relative => {
                let base = base.ok_or(ParseError)?;
                url.scheme = base.scheme.clone();
                if c == Some('/') || (url.is_special() && c == Some('\\')) {
                    state = State::RelativeSlash;
                } else {
                    url.username = base.username.clone();
                    url.password = base.password.clone();
                    url.host = base.host.clone();
                    url.port = base.port;
                    url.path = base.path.clone();
                    url.query = base.query.clone();
                    if c == Some('?') {
                        url.query = Some(String::new());
                        state = State::Query;
                    } else if c == Some('#') {
                        url.fragment = Some(String::new());
                        state = State::Fragment;
                    } else if c.is_some() {
                        url.query = None;
                        url.shorten_path();
                        state = State::Path;
                        pointer -= 1;
                    }
                }
            }
            State::RelativeSlash => {
                if url.is_special() && (c == Some('/') || c == Some('\\')) {
                    state = State::SpecialAuthorityIgnoreSlashes;
                } else if c == Some('/') {
                    state = State::Authority;
                } else {
                    let base = base.ok_or(ParseError)?;
                    url.username = base.username.clone();
                    url.password = base.password.clone();
                    url.host = base.host.clone();
                    url.port = base.port;
                    state = State::Path;
                    pointer -= 1;
                }
            }
            State::SpecialAuthoritySlashes => {
                if c == Some('/') && at(pointer + 1) == Some('/') {
                    state = State::SpecialAuthorityIgnoreSlashes;
                    pointer += 1;
                } else {
                    state = State::SpecialAuthorityIgnoreSlashes;
                    pointer -= 1;
                }
            }
            State::SpecialAuthorityIgnoreSlashes => {
                if c != Some('/') && c != Some('\\') {
                    state = State::Authority;
                    pointer -= 1;
                }
            }
            State::Authority => {
                if c == Some('@') {
                    if at_sign_seen {
                        buffer.insert_str(0, "%40");
                    }
                    at_sign_seen = true;
                    for code_point in buffer.chars() {
                        if code_point == ':' && !password_token_seen {
                            password_token_seen = true;
                            continue;
                        }
                        if password_token_seen {
                            utf8_percent_encode_char(
                                code_point,
                                PercentEncodeSet::Userinfo,
                                &mut url.password,
                            );
                        } else {
                            utf8_percent_encode_char(
                                code_point,
                                PercentEncodeSet::Userinfo,
                                &mut url.username,
                            );
                        }
                    }
                    buffer.clear();
                } else if matches!(c, None | Some('/' | '?' | '#'))
                    || (url.is_special() && c == Some('\\'))
                {
                    if at_sign_seen && buffer.is_empty() {
                        return Err(ParseError);
                    }
                    pointer -= buffer.chars().count() as isize + 1;
                    buffer.clear();
                    state = State::Host;
                } else if let Some(c) = c {
                    buffer.push(c);
                }
            }
            State::Host | State::Hostname => {
                if state_override.is_some() && url.scheme == "file" {
                    pointer -= 1;
                    state = State::FileHost;
                } else if c == Some(':') && !inside_brackets {
                    if buffer.is_empty() {
                        return Err(ParseError);
                    }
                    if state_override == Some(State::Hostname) {
                        return Err(ParseError);
                    }
                    let host = parse_host(&buffer, !url.is_special()).ok_or(ParseError)?;
                    url.host = Some(host);
                    buffer.clear();
                    state = State::Port;
                } else if matches!(c, None | Some('/' | '?' | '#'))
                    || (url.is_special() && c == Some('\\'))
                {
                    pointer -= 1;
                    if url.is_special() && buffer.is_empty() {
                        return Err(ParseError);
                    } else if state_override.is_some()
                        && buffer.is_empty()
                        && (url.includes_credentials() || url.port.is_some())
                    {
                        return Err(ParseError);
                    }
                    let host = if buffer.is_empty() {
                        Host::Empty
                    } else {
                        parse_host(&buffer, !url.is_special()).ok_or(ParseError)?
                    };
                    url.host = Some(host);
                    buffer.clear();
                    state = State::PathStart;
                    if state_override.is_some() {
                        return Ok(());
                    }
                } else if let Some(c) = c {
                    if c == '[' {
                        inside_brackets = true;
                    } else if c == ']' {
                        inside_brackets = false;
                    }
                    buffer.push(c);
                }
            }
            State::Port => {
                if let Some(c) = c.filter(char::is_ascii_digit) {
                    buffer.push(c);
                } else if matches!(c, None | Some('/' | '?' | '#'))
                    || (url.is_special() && c == Some('\\'))
                    || state_override.is_some()
                {
                    if !buffer.is_empty() {
                        let digits = buffer.trim_start_matches('0');
                        let port: u32 = if digits.is_empty() {
                            0
                        } else if digits.len() > 5 {
                            return Err(ParseError);
                        } else {
                            digits.parse().map_err(|_| ParseError)?
                        };
                        if port > 65535 {
                            return Err(ParseError);
                        }
                        let port = port as u16;
                        url.port = if default_port(&url.scheme) == Some(port) {
                            None
                        } else {
                            Some(port)
                        };
                        buffer.clear();
                        if state_override.is_some() {
                            return Ok(());
                        }
                    }
                    if state_override.is_some() {
                        return Err(ParseError);
                    }
                    state = State::PathStart;
                    pointer -= 1;
                } else {
                    return Err(ParseError);
                }
            }
            State::File => {
                url.scheme = "file".to_string();
                url.host = Some(Host::Empty);
                if c == Some('/') || c == Some('\\') {
                    state = State::FileSlash;
                } else if let Some(base) = base.filter(|b| b.scheme == "file") {
                    url.host = base.host.clone();
                    url.path = base.path.clone();
                    url.query = base.query.clone();
                    if c == Some('?') {
                        url.query = Some(String::new());
                        state = State::Query;
                    } else if c == Some('#') {
                        url.fragment = Some(String::new());
                        state = State::Fragment;
                    } else if c.is_some() {
                        url.query = None;
                        if !starts_with_windows_drive_letter(&input[pointer as usize..]) {
                            url.shorten_path();
                        } else {
                            url.path = UrlPath::Segments(Vec::new());
                        }
                        state = State::Path;
                        pointer -= 1;
                    }
                } else {
                    state = State::Path;
                    pointer -= 1;
                }
            }
            State::FileSlash => {
                if c == Some('/') || c == Some('\\') {
                    state = State::FileHost;
                } else {
                    if let Some(base) = base.filter(|b| b.scheme == "file") {
                        url.host = base.host.clone();
                        let rest = &input[pointer.clamp(0, len) as usize..];
                        if !starts_with_windows_drive_letter(rest) {
                            if let UrlPath::Segments(segments) = &base.path {
                                if let Some(first) = segments.first() {
                                    if is_normalized_windows_drive_letter(first) {
                                        url.segments_mut().push(first.clone());
                                    }
                                }
                            }
                        }
                    }
                    state = State::Path;
                    pointer -= 1;
                }
            }
            State::FileHost => {
                if matches!(c, None | Some('/' | '\\' | '?' | '#')) {
                    pointer -= 1;
                    let chars: Vec<char> = buffer.chars().collect();
                    if state_override.is_none() && is_windows_drive_letter(&chars) {
                        // The buffer is not reset: the path state uses it.
                        state = State::Path;
                    } else if buffer.is_empty() {
                        url.host = Some(Host::Empty);
                        if state_override.is_some() {
                            return Ok(());
                        }
                        state = State::PathStart;
                    } else {
                        let mut host = parse_host(&buffer, !url.is_special()).ok_or(ParseError)?;
                        if host == Host::Domain("localhost".to_string()) {
                            host = Host::Empty;
                        }
                        url.host = Some(host);
                        if state_override.is_some() {
                            return Ok(());
                        }
                        buffer.clear();
                        state = State::PathStart;
                    }
                } else if let Some(c) = c {
                    buffer.push(c);
                }
            }
            State::PathStart => {
                if url.is_special() {
                    state = State::Path;
                    if c != Some('/') && c != Some('\\') {
                        pointer -= 1;
                    }
                } else if state_override.is_none() && c == Some('?') {
                    url.query = Some(String::new());
                    state = State::Query;
                } else if state_override.is_none() && c == Some('#') {
                    url.fragment = Some(String::new());
                    state = State::Fragment;
                } else if c.is_some() {
                    state = State::Path;
                    if c != Some('/') {
                        pointer -= 1;
                    }
                } else if state_override.is_some() && url.host.is_none() {
                    url.segments_mut().push(String::new());
                }
            }
            State::Path => {
                let slash = c == Some('/') || (url.is_special() && c == Some('\\'));
                if c.is_none()
                    || slash
                    || (state_override.is_none() && matches!(c, Some('?' | '#')))
                {
                    if is_double_dot_segment(&buffer) {
                        url.shorten_path();
                        if !slash {
                            url.segments_mut().push(String::new());
                        }
                    } else if is_single_dot_segment(&buffer) && !slash {
                        url.segments_mut().push(String::new());
                    } else if !is_single_dot_segment(&buffer) {
                        let chars: Vec<char> = buffer.chars().collect();
                        let empty_path = matches!(&url.path, UrlPath::Segments(s) if s.is_empty());
                        if url.scheme == "file" && empty_path && is_windows_drive_letter(&chars) {
                            buffer = format!("{}:", chars[0]);
                        }
                        let segment = std::mem::take(&mut buffer);
                        url.segments_mut().push(segment);
                    }
                    buffer.clear();
                    if c == Some('?') {
                        url.query = Some(String::new());
                        state = State::Query;
                    }
                    if c == Some('#') {
                        url.fragment = Some(String::new());
                        state = State::Fragment;
                    }
                } else if let Some(c) = c {
                    utf8_percent_encode_char(c, PercentEncodeSet::Path, &mut buffer);
                }
            }
            State::OpaquePath => {
                let UrlPath::Opaque(path) = &mut url.path else {
                    return Err(ParseError);
                };
                match c {
                    Some('?') => {
                        url.query = Some(String::new());
                        state = State::Query;
                    }
                    Some('#') => {
                        url.fragment = Some(String::new());
                        state = State::Fragment;
                    }
                    Some(' ') => {
                        if matches!(at(pointer + 1), Some('?' | '#')) {
                            path.push_str("%20");
                        } else {
                            path.push(' ');
                        }
                    }
                    Some(c) => utf8_percent_encode_char(c, PercentEncodeSet::C0Control, path),
                    None => {}
                }
            }
            State::Query => {
                if encoding != Encoding::Utf8
                    && (!url.is_special() || url.scheme == "ws" || url.scheme == "wss")
                {
                    encoding = Encoding::Utf8;
                }
                if (state_override.is_none() && c == Some('#')) || c.is_none() {
                    let set = if url.is_special() {
                        PercentEncodeSet::SpecialQuery
                    } else {
                        PercentEncodeSet::Query
                    };
                    let encoded = percent_encode_after_encoding(encoding, &buffer, set, false);
                    url.query.get_or_insert_with(String::new).push_str(&encoded);
                    buffer.clear();
                    if c == Some('#') {
                        url.fragment = Some(String::new());
                        state = State::Fragment;
                    }
                } else if let Some(c) = c {
                    buffer.push(c);
                }
            }
            State::Fragment => {
                if let Some(c) = c {
                    let fragment = url.fragment.get_or_insert_with(String::new);
                    utf8_percent_encode_char(c, PercentEncodeSet::Fragment, fragment);
                }
            }
        }
        if pointer >= len {
            break;
        }
        pointer += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn href(input: &str, base: Option<&str>) -> Option<String> {
        let base = base.map(|b| Url::parse(b, None).unwrap());
        Url::parse(input, base.as_ref()).ok().map(|u| u.href())
    }

    #[test]
    fn relative_resolution() {
        let base = Some("http://example.org/foo/bar");
        assert_eq!(
            href("../baz?x#y", base).as_deref(),
            Some("http://example.org/baz?x#y")
        );
        assert_eq!(href("//other/p", base).as_deref(), Some("http://other/p"));
        assert_eq!(
            href("?q", base).as_deref(),
            Some("http://example.org/foo/bar?q")
        );
    }

    #[test]
    fn opaque_paths_keep_spaces_unless_before_query_or_fragment() {
        assert_eq!(
            href("non-special:opaque  ?hi", None).as_deref(),
            Some("non-special:opaque %20?hi")
        );
        assert_eq!(href("mailto:a b", None).as_deref(), Some("mailto:a b"));
    }

    #[test]
    fn file_urls_keep_drive_letters() {
        assert_eq!(
            href("file:///C|/x/../../y", None).as_deref(),
            Some("file:///C:/y")
        );
        assert_eq!(
            href("file://localhost/a", None).as_deref(),
            Some("file:///a")
        );
    }

    #[test]
    fn port_setter_keeps_leading_digits() {
        let mut url = Url::parse("http://h/", None).unwrap();
        url.set_port("8080stuff");
        assert_eq!(url.port, Some(8080));
        url.set_port("80");
        assert_eq!(url.port, None);
    }

    #[test]
    fn origins() {
        let url = Url::parse("https://a.example:8443/x", None).unwrap();
        assert_eq!(url.origin().serialize(), "https://a.example:8443");
        let data = Url::parse("data:,x", None).unwrap();
        assert!(data.origin().is_opaque());
        assert!(!data.origin().same_origin(&data.origin()));
        let blob = Url::parse("blob:https://a.example/uuid", None).unwrap();
        assert_eq!(blob.origin().serialize(), "https://a.example");
    }
}
