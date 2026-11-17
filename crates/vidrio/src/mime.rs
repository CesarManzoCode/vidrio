//! MIME types (WHATWG MIME Sniffing §4): parsing, serialization and the
//! MIME type groups that fetch and navigation ask about.

use std::fmt;

/// A MIME type record: lowercase type and subtype, and parameters with
/// lowercase names in insertion order.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MimeType {
    pub type_: String,
    pub subtype: String,
    pub parameters: Vec<(String, String)>,
}

/// An HTTP whitespace code point (Fetch).
pub fn is_http_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' ')
}

/// An HTTP token code point (MIME Sniffing §2).
pub fn is_http_token_code_point(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

/// An HTTP quoted-string token code point (MIME Sniffing §2).
pub fn is_http_quoted_string_token_code_point(c: char) -> bool {
    matches!(c, '\t' | ' '..='~' | '\u{80}'..='\u{FF}')
}

fn is_token(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_http_token_code_point)
}

/// Collect an HTTP quoted string (Fetch §2.2.2) from `input` at `*position`,
/// which must be `"`, with the extract-value flag set.
pub fn collect_http_quoted_string(input: &[char], position: &mut usize) -> String {
    debug_assert_eq!(input.get(*position), Some(&'"'));
    let mut value = String::new();
    *position += 1;
    loop {
        while let Some(&c) = input.get(*position) {
            if c == '"' || c == '\\' {
                break;
            }
            value.push(c);
            *position += 1;
        }
        let Some(&quote_or_backslash) = input.get(*position) else {
            break;
        };
        *position += 1;
        if quote_or_backslash == '\\' {
            let Some(&escaped) = input.get(*position) else {
                value.push('\\');
                break;
            };
            value.push(escaped);
            *position += 1;
        } else {
            break;
        }
    }
    value
}

impl MimeType {
    /// Parse a MIME type (MIME Sniffing §4.4); `None` on failure.
    pub fn parse(input: &str) -> Option<MimeType> {
        let input: Vec<char> = input.trim_matches(is_http_whitespace).chars().collect();
        let mut position = 0;
        let collect = |position: &mut usize, stop: &dyn Fn(char) -> bool| -> String {
            let start = *position;
            while *position < input.len() && !stop(input[*position]) {
                *position += 1;
            }
            input[start..*position].iter().collect()
        };
        let type_ = collect(&mut position, &|c: char| c == '/');
        if !is_token(&type_) || position >= input.len() {
            return None;
        }
        position += 1;
        let subtype = collect(&mut position, &|c: char| c == ';');
        let subtype = subtype.trim_end_matches(is_http_whitespace);
        if !is_token(subtype) {
            return None;
        }
        let mut mime = MimeType {
            type_: type_.to_ascii_lowercase(),
            subtype: subtype.to_ascii_lowercase(),
            parameters: Vec::new(),
        };
        while position < input.len() {
            // Skip the `;`.
            position += 1;
            collect(&mut position, &|c: char| !is_http_whitespace(c));
            let name = collect(&mut position, &|c: char| c == ';' || c == '=').to_ascii_lowercase();
            if position < input.len() {
                if input[position] == ';' {
                    continue;
                }
                position += 1;
            }
            if position >= input.len() {
                break;
            }
            let value = if input[position] == '"' {
                let value = collect_http_quoted_string(&input, &mut position);
                collect(&mut position, &|c: char| c == ';');
                value
            } else {
                let value = collect(&mut position, &|c: char| c == ';');
                let value = value.trim_end_matches(is_http_whitespace).to_string();
                if value.is_empty() {
                    continue;
                }
                value
            };
            if is_token(&name)
                && value.chars().all(is_http_quoted_string_token_code_point)
                && !mime.parameters.iter().any(|(n, _)| *n == name)
            {
                mime.parameters.push((name, value));
            }
        }
        Some(mime)
    }

    /// Parse a MIME type from bytes (MIME Sniffing §4.4): the isomorphic
    /// decoding of `bytes`, parsed.
    pub fn parse_bytes(bytes: &[u8]) -> Option<MimeType> {
        MimeType::parse(&crate::encoding::isomorphic_decode(bytes))
    }

    /// The essence: `type/subtype`.
    pub fn essence(&self) -> String {
        format!("{}/{}", self.type_, self.subtype)
    }

    /// The value of the parameter `name` (lowercase).
    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Set the parameter `name` (lowercase) to `value`, keeping its place if
    /// it exists.
    pub fn set_parameter(&mut self, name: &str, value: &str) {
        match self.parameters.iter_mut().find(|(n, _)| n == name) {
            Some((_, v)) => *v = value.to_string(),
            None => self.parameters.push((name.to_string(), value.to_string())),
        }
    }

    /// Serialize a MIME type (MIME Sniffing §4.5).
    pub fn serialize(&self) -> String {
        let mut out = self.essence();
        for (name, value) in &self.parameters {
            out.push(';');
            out.push_str(name);
            out.push('=');
            if is_token(value) {
                out.push_str(value);
            } else {
                out.push('"');
                for c in value.chars() {
                    if c == '"' || c == '\\' {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('"');
            }
        }
        out
    }

    /// An image MIME type (§4.6).
    pub fn is_image(&self) -> bool {
        self.type_ == "image"
    }

    /// An audio or video MIME type (§4.6).
    pub fn is_audio_or_video(&self) -> bool {
        self.type_ == "audio" || self.type_ == "video" || self.essence() == "application/ogg"
    }

    /// A font MIME type (§4.6).
    pub fn is_font(&self) -> bool {
        self.type_ == "font"
            || matches!(
                self.essence().as_str(),
                "application/font-cff"
                    | "application/font-off"
                    | "application/font-sfnt"
                    | "application/font-ttf"
                    | "application/font-woff"
                    | "application/vnd.ms-fontobject"
                    | "application/vnd.ms-opentype"
            )
    }

    /// A ZIP-based MIME type (§4.6).
    pub fn is_zip_based(&self) -> bool {
        self.subtype.ends_with("+zip") || self.essence() == "application/zip"
    }

    /// An archive MIME type (§4.6).
    pub fn is_archive(&self) -> bool {
        matches!(
            self.essence().as_str(),
            "application/x-rar-compressed" | "application/zip" | "application/x-gzip"
        )
    }

    /// An XML MIME type (§4.6).
    pub fn is_xml(&self) -> bool {
        self.subtype.ends_with("+xml")
            || matches!(self.essence().as_str(), "text/xml" | "application/xml")
    }

    /// An HTML MIME type (§4.6).
    pub fn is_html(&self) -> bool {
        self.essence() == "text/html"
    }

    /// A scriptable MIME type (§4.6).
    pub fn is_scriptable(&self) -> bool {
        self.is_xml() || self.is_html() || self.essence() == "application/pdf"
    }

    /// A JavaScript MIME type (§4.6).
    pub fn is_javascript(&self) -> bool {
        is_javascript_mime_type_essence(&self.essence())
    }

    /// A JSON MIME type (§4.6).
    pub fn is_json(&self) -> bool {
        self.subtype.ends_with("+json")
            || matches!(self.essence().as_str(), "application/json" | "text/json")
    }
}

/// Whether `essence` is a JavaScript MIME type essence match (§4.6): an
/// ASCII case-insensitive match for one of the JavaScript MIME types.
pub fn is_javascript_mime_type_essence(essence: &str) -> bool {
    matches!(
        essence.to_ascii_lowercase().as_str(),
        "application/ecmascript"
            | "application/javascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "text/javascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    )
}

impl fmt::Display for MimeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.serialize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_lowercases() {
        let m = MimeType::parse(" Text/HTML ; Charset=UTF-8 ; charset=x").unwrap();
        assert_eq!(m.essence(), "text/html");
        assert_eq!(m.parameter("charset"), Some("UTF-8"));
        assert_eq!(m.serialize(), "text/html;charset=UTF-8");
    }

    #[test]
    fn rejects_bad_type_or_subtype() {
        assert_eq!(MimeType::parse("text"), None);
        assert_eq!(MimeType::parse("text/"), None);
        assert_eq!(MimeType::parse("te xt/plain"), None);
        assert_eq!(MimeType::parse("/plain"), None);
    }

    #[test]
    fn empty_and_invalid_parameters_are_dropped() {
        let m = MimeType::parse("a/b;x=;y;z=\u{100};w=1").unwrap();
        assert_eq!(m.serialize(), "a/b;w=1");
    }
}
