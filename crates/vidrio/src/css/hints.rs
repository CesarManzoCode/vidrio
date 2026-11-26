//! The presentational hints of HTML attributes (HTML §15.3–§15.5): the
//! attributes that map to CSS properties (`width`, `height`, `bgcolor`,
//! `align`, `border`, `valign`, `cellspacing`, `cellpadding`, `color`,
//! `face`, `size`, `clear`, `type` of lists, …), with the rules for
//! parsing dimension values, non-negative integers and legacy colors.
//! The cascade treats them as author declarations of specificity zero
//! in a layer before every other author layer. The `hidden` and `dir`
//! attributes are rules of `ua.css`.

use super::declaration::DeclarationBlock;
use super::tokenizer::tokenize;
use super::values::color;
use super::values::number;
use crate::dom::{Document, Namespace, NodeId};

/// A dimension value (HTML §2.3.4.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dimension {
    Length(f64),
    Percentage(f64),
}

impl Dimension {
    fn to_css(self) -> String {
        match self {
            Dimension::Length(v) => format!("{}px", number(v)),
            Dimension::Percentage(v) => format!("{}%", number(v)),
        }
    }
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0C' | '\r')
}

/// The rules for parsing dimension values.
pub fn parse_dimension(input: &str) -> Option<Dimension> {
    let s = input.trim_start_matches(is_space);
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let mut value: f64 = s[..digits].parse().ok()?;
    let mut rest = &s[digits..];
    if let Some(after) = rest.strip_prefix('.') {
        let frac = after.chars().take_while(char::is_ascii_digit).count();
        if frac > 0 {
            let f: f64 = format!("0.{}", &after[..frac]).parse().ok()?;
            value += f;
            rest = &after[frac..];
        } else {
            rest = after;
        }
    }
    Some(if rest.starts_with('%') {
        Dimension::Percentage(value)
    } else {
        Dimension::Length(value)
    })
}

/// The rules for parsing non-zero dimension values.
pub fn parse_nonzero_dimension(input: &str) -> Option<Dimension> {
    match parse_dimension(input)? {
        Dimension::Length(v) | Dimension::Percentage(v) if v == 0.0 => None,
        d => Some(d),
    }
}

/// The rules for parsing non-negative integers.
pub fn parse_non_negative_integer(input: &str) -> Option<u32> {
    let s = input.trim_start_matches(is_space);
    let s = s.strip_prefix('+').unwrap_or(s);
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    Some(s[..digits].parse::<u64>().ok()?.min(u64::from(u32::MAX)) as u32)
}

/// The rules for parsing a legacy color value (HTML §2.3.6), as an
/// `rgb()` serialization.
pub fn parse_legacy_color(input: &str) -> Option<String> {
    let s = input.trim_matches(is_space);
    if s.is_empty() || s.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if let Some(a) = color::named(&s.to_ascii_lowercase()) {
        return Some(a.to_css());
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() == 4 && chars[0] == '#' && chars[1..].iter().all(char::is_ascii_hexdigit) {
        let v: Vec<u8> = chars[1..]
            .iter()
            .map(|c| c.to_digit(16).unwrap_or(0) as u8 * 17)
            .collect();
        return Some(color::Absolute::rgb8(v[0], v[1], v[2], 1.0).to_css());
    }
    let mut buf: Vec<char> = Vec::new();
    for c in chars {
        if u32::from(c) > 0xFFFF {
            buf.push('0');
            buf.push('0');
        } else {
            buf.push(c);
        }
    }
    buf.truncate(128);
    if buf.first() == Some(&'#') {
        buf.remove(0);
    }
    for c in buf.iter_mut() {
        if !c.is_ascii_hexdigit() {
            *c = '0';
        }
    }
    while buf.is_empty() || buf.len() % 3 != 0 {
        buf.push('0');
    }
    let len = buf.len() / 3;
    let mut parts: Vec<Vec<char>> = buf.chunks(len).map(<[char]>::to_vec).collect();
    if len > 8 {
        for p in parts.iter_mut() {
            p.drain(..len - 8);
        }
    }
    while parts[0].len() > 2 && parts.iter().all(|p| p[0] == '0') {
        for p in parts.iter_mut() {
            p.remove(0);
        }
    }
    for p in parts.iter_mut() {
        p.truncate(2);
    }
    let channel = |p: &Vec<char>| {
        let text: String = p.iter().collect();
        u8::from_str_radix(&text, 16).unwrap_or(0)
    };
    Some(
        color::Absolute::rgb8(
            channel(&parts[0]),
            channel(&parts[1]),
            channel(&parts[2]),
            1.0,
        )
        .to_css(),
    )
}

/// The `font-size` keyword of a legacy font size (HTML §15.3.4).
fn legacy_font_size(input: &str) -> Option<&'static str> {
    let s = input.trim_matches(is_space);
    let (mode, digits) = match s.chars().next()? {
        '+' => (1, &s[1..]),
        '-' => (-1, &s[1..]),
        _ => (0, s),
    };
    let n = digits.chars().take_while(char::is_ascii_digit).count();
    if n == 0 {
        return None;
    }
    let v: i64 = digits[..n].parse().ok()?;
    let v = match mode {
        1 => 3 + v,
        -1 => 3 - v,
        _ => v,
    };
    Some(match v.clamp(1, 7) {
        1 => "x-small",
        2 => "small",
        3 => "medium",
        4 => "large",
        5 => "x-large",
        6 => "xx-large",
        _ => "xxx-large",
    })
}

/// The declarations the hints of `element` give.
struct Hints(DeclarationBlock);

impl Hints {
    fn set(&mut self, property: &str, value: &str) {
        self.0.push_raw(property, &tokenize(value), false);
    }

    fn sides(&mut self, pattern: &str, value: &str) {
        for side in ["top", "right", "bottom", "left"] {
            self.set(&pattern.replace("{}", side), value);
        }
    }
}

fn attr<'a>(doc: &'a Document, element: NodeId, name: &str) -> Option<&'a str> {
    doc.get_attribute(element, name)
}

fn eq(v: Option<&str>, word: &str) -> bool {
    v.is_some_and(|v| v.eq_ignore_ascii_case(word))
}

/// The nearest `table` ancestor of a cell or row.
fn table_of(doc: &Document, element: NodeId) -> Option<NodeId> {
    let mut current = doc.parent(element);
    while let Some(n) = current {
        if doc.is_html_element(n, "table") {
            return Some(n);
        }
        current = doc.parent(n);
    }
    None
}

/// The `border` attribute of a table in pixels: present but unparsable
/// counts as 1.
fn table_border(doc: &Document, table: NodeId) -> Option<u32> {
    let v = attr(doc, table, "border")?;
    Some(parse_non_negative_integer(v).unwrap_or(1))
}

/// The presentational hints of `element`; an empty block for elements
/// that are not HTML or have no mapped attribute.
pub fn presentational_hints(doc: &Document, element: NodeId) -> DeclarationBlock {
    let mut h = Hints(DeclarationBlock::default());
    let Some(e) = doc.element(element) else {
        return h.0;
    };
    if e.name.ns != Some(Namespace::Html) {
        return h.0;
    }
    let local = e.name.local.as_str();
    let a = |name: &str| attr(doc, element, name);

    // Background colors and images.
    if matches!(
        local,
        "body" | "table" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th"
    ) {
        if let Some(c) = a("bgcolor").and_then(parse_legacy_color) {
            h.set("background-color", &c);
        }
        if let Some(url) = a("background").filter(|v| !v.trim().is_empty()) {
            h.set(
                "background-image",
                &format!("url({})", super::declaration::serialize_string(url.trim())),
            );
        }
    }

    match local {
        "body" => {
            if let Some(c) = a("text").and_then(parse_legacy_color) {
                h.set("color", &c);
            }
            let px = |names: &[&str]| {
                names
                    .iter()
                    .find_map(|n| a(n).and_then(parse_non_negative_integer))
            };
            for (side, names) in [
                ("top", ["marginheight", "topmargin"]),
                ("bottom", ["marginheight", "bottommargin"]),
                ("left", ["marginwidth", "leftmargin"]),
                ("right", ["marginwidth", "rightmargin"]),
            ] {
                if let Some(v) = px(&names) {
                    h.set(&format!("margin-{side}"), &format!("{v}px"));
                }
            }
        }
        "table" => {
            if let Some(d) = a("width").and_then(parse_nonzero_dimension) {
                h.set("width", &d.to_css());
            }
            if let Some(d) = a("height").and_then(parse_dimension) {
                h.set("height", &d.to_css());
            }
            if let Some(v) = a("cellspacing").and_then(parse_non_negative_integer) {
                h.set("border-spacing", &format!("{v}px"));
            }
            if let Some(b) = table_border(doc, element) {
                h.sides("border-{}-width", &format!("{b}px"));
                if b > 0 {
                    h.sides("border-{}-style", "outset");
                }
            }
            match a("align").map(str::to_ascii_lowercase).as_deref() {
                Some("left") => h.set("float", "left"),
                Some("right") => h.set("float", "right"),
                Some("center") => {
                    h.set("margin-left", "auto");
                    h.set("margin-right", "auto");
                }
                _ => {}
            }
        }
        "td" | "th" => {
            if let Some(d) = a("width").and_then(parse_nonzero_dimension) {
                h.set("width", &d.to_css());
            }
            if let Some(d) = a("height").and_then(parse_nonzero_dimension) {
                h.set("height", &d.to_css());
            }
            if a("nowrap").is_some() {
                h.set("text-wrap-mode", "nowrap");
            }
            if let Some(table) = table_of(doc, element) {
                if table_border(doc, table).is_some_and(|b| b > 0) {
                    h.sides("border-{}-width", "1px");
                    h.sides("border-{}-style", "inset");
                }
                if let Some(v) =
                    attr(doc, table, "cellpadding").and_then(parse_non_negative_integer)
                {
                    h.sides("padding-{}", &format!("{v}px"));
                }
            }
        }
        "col" => {
            if let Some(d) = a("width").and_then(parse_nonzero_dimension) {
                h.set("width", &d.to_css());
            }
        }
        "caption" => match a("align").map(str::to_ascii_lowercase).as_deref() {
            Some("bottom") => h.set("caption-side", "bottom"),
            Some("top") => h.set("caption-side", "top"),
            _ => {}
        },
        "hr" => {
            match a("align").map(str::to_ascii_lowercase).as_deref() {
                Some("left") => {
                    h.set("margin-left", "0");
                    h.set("margin-right", "auto");
                }
                Some("right") => {
                    h.set("margin-left", "auto");
                    h.set("margin-right", "0");
                }
                Some("center") => {
                    h.set("margin-left", "auto");
                    h.set("margin-right", "auto");
                }
                _ => {}
            }
            if let Some(c) = a("color").and_then(parse_legacy_color) {
                h.sides("border-{}-style", "solid");
                h.set("color", &c);
                h.set("background-color", &c);
            } else if a("noshade").is_some() {
                h.sides("border-{}-style", "solid");
                h.sides("border-{}-color", "gray");
                h.set("background-color", "gray");
            }
            if let Some(size) = a("size").and_then(parse_non_negative_integer) {
                if size == 1 {
                    h.set("border-bottom-width", "0");
                } else if size > 1 {
                    h.set("height", &format!("{}px", size - 2));
                }
            }
            if let Some(d) = a("width").and_then(parse_nonzero_dimension) {
                h.set("width", &d.to_css());
            }
        }
        "font" => {
            if let Some(c) = a("color").and_then(parse_legacy_color) {
                h.set("color", &c);
            }
            if let Some(face) = a("face").filter(|f| !f.trim().is_empty()) {
                h.set("font-family", face);
            }
            if let Some(size) = a("size").and_then(legacy_font_size) {
                h.set("font-size", size);
            }
        }
        "br" => match a("clear").map(str::to_ascii_lowercase).as_deref() {
            Some("left") => h.set("clear", "left"),
            Some("right") => h.set("clear", "right"),
            Some("all") | Some("both") => h.set("clear", "both"),
            _ => {}
        },
        "pre" if a("wrap").is_some() => h.set("text-wrap-mode", "wrap"),
        "ol" | "ul" | "li" => {
            if let Some(t) = a("type") {
                let style = match t.trim() {
                    "1" => Some("decimal"),
                    "a" => Some("lower-alpha"),
                    "A" => Some("upper-alpha"),
                    "i" => Some("lower-roman"),
                    "I" => Some("upper-roman"),
                    other => match other.to_ascii_lowercase().as_str() {
                        "none" => Some("none"),
                        "disc" => Some("disc"),
                        "circle" => Some("circle"),
                        "square" => Some("square"),
                        _ => None,
                    },
                };
                if let Some(style) = style {
                    h.set("list-style-type", style);
                }
            }
        }
        _ => {}
    }

    // `align` as text alignment.
    if matches!(
        local,
        "div"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "legend"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
    ) {
        match a("align").map(str::to_ascii_lowercase).as_deref() {
            Some("left") => h.set("text-align", "left"),
            Some("right") => h.set("text-align", "right"),
            Some("center") | Some("middle") => h.set("text-align", "center"),
            Some("justify") => h.set("text-align", "justify"),
            _ => {}
        }
    }

    // `valign` of table parts.
    if matches!(
        local,
        "col" | "colgroup" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th"
    ) {
        if let Some(v) = a("valign").map(str::to_ascii_lowercase) {
            if matches!(v.as_str(), "top" | "middle" | "bottom" | "baseline") {
                h.set("vertical-align", &v);
            }
        }
    }

    // Embedded content and images.
    let is_image_input = local == "input" && eq(a("type"), "image");
    if matches!(
        local,
        "img" | "object" | "embed" | "iframe" | "video" | "canvas"
    ) || is_image_input
    {
        if local != "canvas" {
            if let Some(d) = a("width").and_then(parse_dimension) {
                h.set("width", &d.to_css());
            }
            if let Some(d) = a("height").and_then(parse_dimension) {
                h.set("height", &d.to_css());
            }
        }
        if let Some(d) = a("hspace").and_then(parse_dimension) {
            h.set("margin-left", &d.to_css());
            h.set("margin-right", &d.to_css());
        }
        if let Some(d) = a("vspace").and_then(parse_dimension) {
            h.set("margin-top", &d.to_css());
            h.set("margin-bottom", &d.to_css());
        }
        if matches!(local, "img" | "object") || is_image_input {
            if let Some(b) = a("border").and_then(parse_non_negative_integer) {
                h.sides("border-{}-width", &format!("{b}px"));
                h.sides("border-{}-style", "solid");
            }
        }
        match a("align").map(str::to_ascii_lowercase).as_deref() {
            Some("left") => h.set("float", "left"),
            Some("right") => h.set("float", "right"),
            Some("top") => h.set("vertical-align", "top"),
            Some("middle") | Some("center") => h.set("vertical-align", "middle"),
            Some("bottom") | Some("baseline") => h.set("vertical-align", "baseline"),
            _ => {}
        }
    }
    h.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_colors() {
        assert_eq!(
            parse_legacy_color("chucknorris").as_deref(),
            Some("rgb(192, 0, 0)")
        );
        assert_eq!(
            parse_legacy_color("#0f0").as_deref(),
            Some("rgb(0, 255, 0)")
        );
        assert_eq!(parse_legacy_color("Red").as_deref(), Some("rgb(255, 0, 0)"));
        assert_eq!(parse_legacy_color("transparent"), None);
    }

    #[test]
    fn dimensions() {
        assert_eq!(parse_dimension(" 50%"), Some(Dimension::Percentage(50.0)));
        assert_eq!(parse_dimension("12.5px"), Some(Dimension::Length(12.5)));
        assert_eq!(parse_nonzero_dimension("0"), None);
        assert_eq!(parse_non_negative_integer("+7x"), Some(7));
    }

    #[test]
    fn hints_of_attributes() {
        let mut doc = Document::new_html();
        let img = doc.create_html_element("img");
        doc.set_attribute(img, "width", "100");
        doc.set_attribute(img, "border", "2");
        let block = presentational_hints(&doc, img);
        assert_eq!(block.property_value("width"), "100px");
        assert_eq!(block.property_value("border-top-width"), "2px");
        let col = doc.create_html_element("col");
        doc.set_attribute(col, "width", "25%");
        assert_eq!(
            presentational_hints(&doc, col).property_value("width"),
            "25%"
        );
        let font = doc.create_html_element("font");
        doc.set_attribute(font, "size", "+2");
        assert_eq!(
            presentational_hints(&doc, font).property_value("font-size"),
            "x-large"
        );
    }
}
