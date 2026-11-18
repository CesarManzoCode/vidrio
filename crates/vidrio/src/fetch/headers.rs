//! Header lists (Fetch §2.2.2) and the `Headers` class with its guards
//! (Fetch §5.1). Names and values are byte sequences; they are kept as
//! Rust strings whose characters are all below U+0100 (the IDL
//! `ByteString`s they come from).

use crate::mime::MimeType;

/// A header list: name/value pairs in order, names as given.
pub type HeaderList = Vec<(String, String)>;

/// The guard of a `Headers` object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    Immutable,
    Request,
    RequestNoCors,
    Response,
    None,
}

/// HTTP whitespace bytes (Fetch §2.2).
fn is_http_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' ')
}

/// "Normalize" a header value: strip leading and trailing HTTP
/// whitespace.
pub fn normalize(value: &str) -> String {
    value.trim_matches(is_http_ws).to_string()
}

/// A header name: an HTTP token.
pub fn is_header_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(crate::mime::is_http_token_code_point)
}

/// A header value: no leading or trailing HTTP tab or space, no NUL, CR
/// or LF.
pub fn is_header_value(value: &str) -> bool {
    let edges = |c: Option<char>| !matches!(c, Some('\t' | ' '));
    edges(value.chars().next())
        && edges(value.chars().next_back())
        && !value.chars().any(|c| matches!(c, '\0' | '\r' | '\n'))
}

/// The combined value of `name` in `list` ("get", Fetch §2.2.2).
pub fn get(list: &HeaderList, name: &str) -> Option<String> {
    let values: Vec<&str> = list
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values.join(", "))
    }
}

pub fn contains(list: &HeaderList, name: &str) -> bool {
    list.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// "Append": a second header of the same name takes the first one's case.
pub fn append(list: &mut HeaderList, name: &str, value: &str) {
    let name = list
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map_or_else(|| name.to_string(), |(n, _)| n.clone());
    list.push((name, value.to_string()));
}

pub fn delete(list: &mut HeaderList, name: &str) {
    list.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
}

/// "Set": the first header of the name takes the value, the others go.
pub fn set(list: &mut HeaderList, name: &str, value: &str) {
    match list.iter().position(|(n, _)| n.eq_ignore_ascii_case(name)) {
        Some(i) => {
            list[i].1 = value.to_string();
            let mut seen = false;
            list.retain(|(n, _)| {
                if !n.eq_ignore_ascii_case(name) {
                    return true;
                }
                let keep = !seen;
                seen = true;
                keep
            });
        }
        None => list.push((name.to_string(), value.to_string())),
    }
}

/// "Combine": append `value` to the existing value with `, `.
pub fn combine(list: &mut HeaderList, name: &str, value: &str) {
    match list.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        Some(entry) => {
            entry.1.push_str(", ");
            entry.1.push_str(value);
        }
        None => list.push((name.to_string(), value.to_string())),
    }
}

/// "Sort and combine": lowercase names in order, values combined, each
/// `set-cookie` on its own.
pub fn sort_and_combine(list: &HeaderList) -> Vec<(String, String)> {
    let mut names: Vec<String> = list.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
    names.sort();
    names.dedup();
    let mut out = Vec::new();
    for name in names {
        if name == "set-cookie" {
            for (n, v) in list {
                if n.eq_ignore_ascii_case("set-cookie") {
                    out.push((name.clone(), v.clone()));
                }
            }
        } else if let Some(v) = get(list, &name) {
            out.push((name, v));
        }
    }
    out
}

/// The values of a comma-separated header value ("get, decode, and
/// split", without quoted strings).
pub fn split_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|v| v.trim_matches(is_http_ws).to_string())
        .collect()
}

/// A forbidden method (Fetch §2.2.1).
pub fn is_forbidden_method(method: &str) -> bool {
    ["CONNECT", "TRACE", "TRACK"]
        .iter()
        .any(|m| m.eq_ignore_ascii_case(method))
}

/// "Normalize" a method: the six standard ones are uppercased.
pub fn normalize_method(method: &str) -> String {
    for m in ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"] {
        if m.eq_ignore_ascii_case(method) {
            return m.to_string();
        }
    }
    method.to_string()
}

/// A CORS-safelisted method.
pub fn is_cors_safelisted_method(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "POST")
}

/// A forbidden request-header (Fetch §2.2.2).
pub fn is_forbidden_request_header(name: &str, value: &str) -> bool {
    const NAMES: [&str; 21] = [
        "accept-charset",
        "accept-encoding",
        "access-control-request-headers",
        "access-control-request-method",
        "connection",
        "content-length",
        "cookie",
        "cookie2",
        "date",
        "dnt",
        "expect",
        "host",
        "keep-alive",
        "origin",
        "referer",
        "set-cookie",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "via",
    ];
    let lower = name.to_ascii_lowercase();
    if NAMES.contains(&lower.as_str()) || lower.starts_with("proxy-") || lower.starts_with("sec-") {
        return true;
    }
    if matches!(
        lower.as_str(),
        "x-http-method" | "x-http-method-override" | "x-method-override"
    ) {
        return split_values(value).iter().any(|m| is_forbidden_method(m));
    }
    false
}

/// A forbidden response-header name.
pub fn is_forbidden_response_header_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("set-cookie") || name.eq_ignore_ascii_case("set-cookie2")
}

/// A byte that makes a header value CORS-unsafe.
fn is_cors_unsafe_byte(c: char) -> bool {
    (c < ' ' && c != '\t')
        || matches!(
            c,
            '"' | '(' | ')' | ':' | '<' | '>' | '?' | '@' | '[' | '\\' | ']' | '{' | '}' | '\u{7F}'
        )
}

/// A CORS-safelisted request-header (Fetch §2.2.2).
pub fn is_cors_safelisted_request_header(name: &str, value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    match name.to_ascii_lowercase().as_str() {
        "accept" => !value.chars().any(is_cors_unsafe_byte),
        "accept-language" | "content-language" => value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " *,-.;=".contains(c)),
        "content-type" => {
            if value.chars().any(is_cors_unsafe_byte) {
                return false;
            }
            MimeType::parse(value).is_some_and(|m| {
                matches!(
                    m.essence().as_str(),
                    "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
                )
            })
        }
        "range" => is_simple_range(value),
        _ => false,
    }
}

/// `bytes=N-` or `bytes=N-M` (the simple range header value).
fn is_simple_range(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("bytes=") else {
        return false;
    };
    let Some((a, b)) = rest.split_once('-') else {
        return false;
    };
    let digits = |s: &str| s.bytes().all(|c| c.is_ascii_digit());
    if a.is_empty() || !digits(a) || !digits(b) {
        return false;
    }
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x <= y,
        (Ok(_), Err(_)) => b.is_empty(),
        _ => false,
    }
}

/// A no-CORS-safelisted request-header name.
fn is_no_cors_safelisted_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "accept" | "accept-language" | "content-language" | "content-type"
    )
}

/// A privileged no-CORS request-header name.
fn is_privileged_no_cors_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("range")
}

/// The CORS-unsafe request-header names of `list`: lowercase, sorted,
/// without duplicates (Fetch §2.2.2).
pub fn cors_unsafe_names(list: &HeaderList) -> Vec<String> {
    let mut unsafe_names = Vec::new();
    let mut safelisted_len = 0;
    for (n, v) in list {
        if is_cors_safelisted_request_header(n, v) {
            safelisted_len += v.len();
        } else {
            unsafe_names.push(n.to_ascii_lowercase());
        }
    }
    if safelisted_len > 1024 {
        for (n, v) in list {
            if is_cors_safelisted_request_header(n, v) {
                unsafe_names.push(n.to_ascii_lowercase());
            }
        }
    }
    unsafe_names.sort();
    unsafe_names.dedup();
    unsafe_names
}

/// A CORS-safelisted response-header name, given the exposed names of
/// `Access-Control-Expose-Headers`.
pub fn is_cors_safelisted_response_header_name(name: &str, exposed: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "cache-control"
            | "content-language"
            | "content-length"
            | "content-type"
            | "expires"
            | "last-modified"
            | "pragma"
    ) || (!is_forbidden_response_header_name(name)
        && exposed.iter().any(|e| e.eq_ignore_ascii_case(name)))
}

/// A `Headers` object's header list and guard.
#[derive(Clone, Debug)]
pub struct Headers {
    pub list: HeaderList,
    pub guard: Guard,
}

/// Why an operation on [`Headers`] failed: a `TypeError` with this
/// message.
pub type HeadersError = String;

fn validate(name: &str, value: Option<&str>) -> Result<(), HeadersError> {
    if !is_header_name(name) {
        return Err(format!("'{name}' is not a valid header name."));
    }
    if let Some(v) = value {
        if !is_header_value(v) {
            return Err(format!("'{v}' is not a valid header value."));
        }
    }
    Ok(())
}

impl Headers {
    pub fn new(guard: Guard) -> Headers {
        Headers {
            list: Vec::new(),
            guard,
        }
    }

    /// The guard check of "validate" (true: go on).
    fn allowed(&self, name: &str, value: &str) -> Result<bool, HeadersError> {
        validate(name, Some(value))?;
        match self.guard {
            Guard::Immutable => Err("Headers are immutable.".to_string()),
            Guard::Request if is_forbidden_request_header(name, value) => Ok(false),
            Guard::Response if is_forbidden_response_header_name(name) => Ok(false),
            _ => Ok(true),
        }
    }

    fn remove_privileged_no_cors(&mut self) {
        if self.guard == Guard::RequestNoCors {
            self.list.retain(|(n, _)| !is_privileged_no_cors_name(n));
        }
    }

    /// `append(name, value)` (Fetch §5.1).
    pub fn append(&mut self, name: &str, value: &str) -> Result<(), HeadersError> {
        let value = normalize(value);
        if !self.allowed(name, &value)? {
            return Ok(());
        }
        if self.guard == Guard::RequestNoCors {
            let temporary = match get(&self.list, name) {
                Some(t) => format!("{t}, {value}"),
                None => value.clone(),
            };
            if !(is_no_cors_safelisted_name(name)
                && is_cors_safelisted_request_header(name, &temporary))
            {
                return Ok(());
            }
        }
        append(&mut self.list, name, &value);
        self.remove_privileged_no_cors();
        Ok(())
    }

    /// `delete(name)`.
    pub fn delete(&mut self, name: &str) -> Result<(), HeadersError> {
        if !self.allowed(name, "")? {
            return Ok(());
        }
        if self.guard == Guard::RequestNoCors
            && !is_no_cors_safelisted_name(name)
            && !is_privileged_no_cors_name(name)
        {
            return Ok(());
        }
        if !contains(&self.list, name) {
            return Ok(());
        }
        delete(&mut self.list, name);
        self.remove_privileged_no_cors();
        Ok(())
    }

    /// `get(name)`.
    pub fn get(&self, name: &str) -> Result<Option<String>, HeadersError> {
        validate(name, None)?;
        Ok(get(&self.list, name))
    }

    /// `has(name)`.
    pub fn has(&self, name: &str) -> Result<bool, HeadersError> {
        validate(name, None)?;
        Ok(contains(&self.list, name))
    }

    /// `getSetCookie()`.
    pub fn get_set_cookie(&self) -> Vec<String> {
        self.list
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// `set(name, value)`.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), HeadersError> {
        let value = normalize(value);
        if !self.allowed(name, &value)? {
            return Ok(());
        }
        if self.guard == Guard::RequestNoCors
            && !(is_no_cors_safelisted_name(name)
                && is_cors_safelisted_request_header(name, &value))
        {
            return Ok(());
        }
        set(&mut self.list, name, &value);
        self.remove_privileged_no_cors();
        Ok(())
    }

    /// "Fill" from a sequence of pairs or a record.
    pub fn fill(&mut self, pairs: &[(String, String)]) -> Result<(), HeadersError> {
        for (n, v) in pairs {
            self.append(n, v)?;
        }
        Ok(())
    }

    /// The pairs the iterator walks: "sort and combine".
    pub fn iteration_pairs(&self) -> Vec<(String, String)> {
        sort_and_combine(&self.list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guards_filter_forbidden_headers() {
        let mut h = Headers::new(Guard::Request);
        h.append("Content-Type", " text/plain ").unwrap();
        h.append("Cookie", "a=b").unwrap();
        h.append("Sec-Foo", "x").unwrap();
        h.append("X-Custom", "1").unwrap();
        h.append("x-custom", "2").unwrap();
        assert_eq!(
            h.get("content-type").unwrap().as_deref(),
            Some("text/plain")
        );
        assert!(!h.has("cookie").unwrap());
        assert!(!h.has("sec-foo").unwrap());
        assert_eq!(h.get("X-CUSTOM").unwrap().as_deref(), Some("1, 2"));
        assert!(h.append("bad name", "v").is_err());
        assert!(h.append("x", "a\nb").is_err());

        let mut r = Headers::new(Guard::Response);
        r.append("Set-Cookie", "a=b").unwrap();
        assert!(!r.has("set-cookie").unwrap());

        let mut i = Headers::new(Guard::Immutable);
        assert!(i.append("x", "y").is_err());

        let mut n = Headers::new(Guard::RequestNoCors);
        n.append("Accept", "text/html").unwrap();
        n.append("X-Custom", "1").unwrap();
        n.append("Content-Type", "application/json").unwrap();
        assert!(n.has("accept").unwrap());
        assert!(!n.has("x-custom").unwrap());
        assert!(!n.has("content-type").unwrap());
    }

    #[test]
    fn sort_and_combine_keeps_set_cookie_apart() {
        let list = vec![
            ("B".to_string(), "1".to_string()),
            ("set-cookie".to_string(), "x".to_string()),
            ("a".to_string(), "2".to_string()),
            ("b".to_string(), "3".to_string()),
            ("Set-Cookie".to_string(), "y".to_string()),
        ];
        assert_eq!(
            sort_and_combine(&list),
            vec![
                ("a".to_string(), "2".to_string()),
                ("b".to_string(), "1, 3".to_string()),
                ("set-cookie".to_string(), "x".to_string()),
                ("set-cookie".to_string(), "y".to_string()),
            ]
        );
    }

    #[test]
    fn cors_safelisting() {
        assert!(is_cors_safelisted_request_header(
            "Content-Type",
            "text/plain;charset=UTF-8"
        ));
        assert!(!is_cors_safelisted_request_header(
            "Content-Type",
            "application/json"
        ));
        assert!(is_cors_safelisted_request_header("Range", "bytes=0-10"));
        assert!(!is_cors_safelisted_request_header("Range", "bytes=10-0"));
        let list = vec![
            ("X-B".to_string(), "1".to_string()),
            ("Accept".to_string(), "*/*".to_string()),
            ("x-a".to_string(), "2".to_string()),
        ];
        assert_eq!(cors_unsafe_names(&list), vec!["x-a", "x-b"]);
        assert!(is_forbidden_request_header(
            "X-HTTP-Method-Override",
            "get, TRACE"
        ));
        assert!(is_cors_safelisted_response_header_name("Content-Type", &[]));
        assert!(is_cors_safelisted_response_header_name(
            "x-y",
            &["X-Y".to_string()]
        ));
        assert!(!is_cors_safelisted_response_header_name("x-y", &[]));
    }
}
