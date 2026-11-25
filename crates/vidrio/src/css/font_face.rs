//! The descriptors of `@font-face` (CSS Fonts 4 §4): `font-family`,
//! `src` (`url()` with `format()` and `tech()`, `local()`),
//! `unicode-range`, and the ranges of `font-weight`, `font-stretch` and
//! `font-style`, parsed into what font matching uses
//! (`vgfx::font::matching`). The `FontFace` interface parses its
//! attributes with the same functions.

use vgfx::font::matching::{Descriptor, StyleRange};

use super::tokenizer::{Token, tokenize};

/// One entry of `src`.
#[derive(Clone, Debug, PartialEq)]
pub enum FontSource {
    Url {
        url: String,
        format: Option<String>,
        tech: Vec<String>,
    },
    Local(String),
}

/// The formats vidrio decodes (CSS Fonts 4 §4.3.1, `<font-format>`).
const FORMATS: &[&str] = &["woff", "woff2", "truetype", "opentype", "collection"];

/// The technologies vidrio supports (`<font-tech>`).
const TECHS: &[&str] = &["features-opentype"];

fn significant(v: &str) -> Vec<Token> {
    tokenize(v)
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace))
        .collect()
}

/// Splits tokens on top-level commas (inside functions commas stay).
fn split_commas(tokens: &[Token]) -> Vec<&[Token]> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, t) in tokens.iter().enumerate() {
        match t {
            Token::Function(_) | Token::LeftParen | Token::LeftBracket | Token::LeftBrace => {
                depth += 1
            }
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth = depth.saturating_sub(1)
            }
            Token::Comma if depth == 0 => {
                out.push(&tokens[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&tokens[start..]);
    out
}

/// A `<family-name>`: a string, or a sequence of identifiers that is not
/// a CSS-wide keyword or `default` alone, nor a generic family.
pub fn parse_family_name(value: &str) -> Option<String> {
    let tokens = significant(value);
    family_name(&tokens, true)
}

fn family_name(tokens: &[Token], reject_generic: bool) -> Option<String> {
    match tokens {
        [Token::String(s)] => Some(s.clone()),
        [] => None,
        idents => {
            let mut words = Vec::new();
            for t in idents {
                match t {
                    Token::Ident(w) => words.push(w.clone()),
                    _ => return None,
                }
            }
            if words.len() == 1 {
                let w = words[0].to_ascii_lowercase();
                let reserved = matches!(
                    w.as_str(),
                    "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "default"
                );
                let generic = matches!(
                    w.as_str(),
                    "serif"
                        | "sans-serif"
                        | "monospace"
                        | "cursive"
                        | "fantasy"
                        | "system-ui"
                        | "math"
                        | "emoji"
                        | "fangsong"
                        | "ui-serif"
                        | "ui-sans-serif"
                        | "ui-monospace"
                        | "ui-rounded"
                );
                if reserved || (reject_generic && generic) {
                    return None;
                }
            }
            Some(words.join(" "))
        }
    }
}

/// A `font-family` property value: family names and generic keywords.
pub fn parse_family_list(value: &str) -> Option<Vec<String>> {
    let tokens = significant(value);
    split_commas(&tokens)
        .into_iter()
        .map(|part| family_name(part, false))
        .collect()
}

/// Parses `src` (CSS Fonts 4 §4.3). Entries with an unsupported
/// `format()` or `tech()` are dropped, as the descriptor's processing
/// skips them; `None` if the value is invalid or nothing is left.
pub fn parse_src(value: &str) -> Option<Vec<FontSource>> {
    let tokens = significant(value);
    let mut out = Vec::new();
    for part in split_commas(&tokens) {
        let (source, rest) = match part {
            [Token::Url(u), rest @ ..] => (Some(u.clone()), rest),
            [
                Token::Function(f),
                Token::String(u),
                Token::RightParen,
                rest @ ..,
            ] if f.eq_ignore_ascii_case("url") => (Some(u.clone()), rest),
            [Token::Function(f), name @ .., Token::RightParen]
                if f.eq_ignore_ascii_case("local") =>
            {
                out.push(FontSource::Local(family_name(name, false)?));
                continue;
            }
            _ => return None,
        };
        let url = source?;
        let mut format = None;
        let mut tech = Vec::new();
        let mut r = rest;
        let mut supported = true;
        while let [Token::Function(f), ..] = r {
            let close = r.iter().position(|t| *t == Token::RightParen)?;
            let args = &r[1..close];
            let name = f.to_ascii_lowercase();
            match name.as_str() {
                "format" if format.is_none() && tech.is_empty() => {
                    let fmt = match args {
                        [Token::String(s)] | [Token::Ident(s)] => s.to_ascii_lowercase(),
                        _ => return None,
                    };
                    supported &= FORMATS.contains(&fmt.as_str());
                    format = Some(fmt);
                }
                "tech" if tech.is_empty() => {
                    for a in split_commas(args) {
                        match a {
                            [Token::Ident(t)] => tech.push(t.to_ascii_lowercase()),
                            _ => return None,
                        }
                    }
                    if tech.is_empty() {
                        return None;
                    }
                    supported &= tech.iter().all(|t| TECHS.contains(&t.as_str()));
                }
                _ => return None,
            }
            r = &r[close + 1..];
        }
        if !r.is_empty() {
            return None;
        }
        if supported {
            out.push(FontSource::Url { url, format, tech });
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Parses `unicode-range` (CSS Fonts 4 §4.5, with the `<urange>` syntax
/// of CSS Syntax 3 §7.1) into inclusive ranges.
pub fn parse_unicode_range(value: &str) -> Option<Vec<(u32, u32)>> {
    let mut out = Vec::new();
    for part in value.split(',') {
        let p = part.trim();
        let rest = p.strip_prefix(['u', 'U'])?.strip_prefix('+')?;
        let (first, second) = match rest.split_once('-') {
            Some((a, b)) => (a, Some(b)),
            None => (rest, None),
        };
        let hex = |s: &str| -> Option<u32> {
            if s.is_empty() || s.len() > 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            u32::from_str_radix(s, 16).ok()
        };
        let (lo, hi) = match second {
            Some(b) => (hex(first)?, hex(b)?),
            None if first.contains('?') => {
                let digits = first.trim_end_matches('?');
                let wild = first.len() - digits.len();
                if first.len() > 6 || digits.contains('?') || wild == 0 {
                    return None;
                }
                let base = if digits.is_empty() { 0 } else { hex(digits)? };
                let shift = 4 * wild as u32;
                (base << shift, (base << shift) | ((1 << shift) - 1))
            }
            None => {
                let v = hex(first)?;
                (v, v)
            }
        };
        if hi > 0x10ffff || lo > hi {
            return None;
        }
        out.push((lo, hi));
    }
    Some(out)
}

fn number_of(t: &Token) -> Option<f32> {
    match t {
        Token::Number(n) => Some(n.value as f32),
        _ => None,
    }
}

/// `font-weight` descriptor: `auto`, or one or two of `normal`, `bold`,
/// `<number [1,1000]>`.
pub fn parse_weight(value: &str) -> Option<(f32, f32)> {
    let tokens = significant(value);
    let one = |t: &Token| -> Option<f32> {
        match t {
            Token::Ident(k) if k.eq_ignore_ascii_case("normal") => Some(400.0),
            Token::Ident(k) if k.eq_ignore_ascii_case("bold") => Some(700.0),
            t => number_of(t).filter(|v| (1.0..=1000.0).contains(v)),
        }
    };
    match tokens.as_slice() {
        [Token::Ident(k)] if k.eq_ignore_ascii_case("auto") => Some((1.0, 1000.0)),
        [a] => one(a).map(|v| (v, v)),
        [a, b] => {
            let (a, b) = (one(a)?, one(b)?);
            Some((a.min(b), a.max(b)))
        }
        _ => None,
    }
}

/// `font-stretch` (`font-width`) descriptor in percent.
pub fn parse_stretch(value: &str) -> Option<(f32, f32)> {
    let tokens = significant(value);
    let one = |t: &Token| -> Option<f32> {
        match t {
            Token::Percentage(p) if p.value >= 0.0 => Some(p.value as f32),
            Token::Ident(k) => Some(match k.to_ascii_lowercase().as_str() {
                "ultra-condensed" => 50.0,
                "extra-condensed" => 62.5,
                "condensed" => 75.0,
                "semi-condensed" => 87.5,
                "normal" => 100.0,
                "semi-expanded" => 112.5,
                "expanded" => 125.0,
                "extra-expanded" => 150.0,
                "ultra-expanded" => 200.0,
                _ => return None,
            }),
            _ => None,
        }
    };
    match tokens.as_slice() {
        [Token::Ident(k)] if k.eq_ignore_ascii_case("auto") => Some((50.0, 200.0)),
        [a] => one(a).map(|v| (v, v)),
        [a, b] => {
            let (a, b) = (one(a)?, one(b)?);
            Some((a.min(b), a.max(b)))
        }
        _ => None,
    }
}

fn angle_of(t: &Token) -> Option<f32> {
    let Token::Dimension(n, unit) = t else {
        return match t {
            Token::Number(n) if n.value == 0.0 => Some(0.0),
            _ => None,
        };
    };
    let v = n.value as f32;
    let deg = match unit.to_ascii_lowercase().as_str() {
        "deg" => v,
        "grad" => v * 0.9,
        "rad" => v.to_degrees(),
        "turn" => v * 360.0,
        _ => return None,
    };
    (-90.0..=90.0).contains(&deg).then_some(deg)
}

/// `font-style` descriptor.
pub fn parse_style(value: &str) -> Option<StyleRange> {
    let tokens = significant(value);
    match tokens.as_slice() {
        [Token::Ident(k)] => match k.to_ascii_lowercase().as_str() {
            "normal" => Some(StyleRange::Normal),
            "italic" => Some(StyleRange::Italic),
            "oblique" => Some(StyleRange::Oblique(14.0, 14.0)),
            "auto" => Some(StyleRange::Oblique(-90.0, 90.0)),
            _ => None,
        },
        [Token::Ident(k), a] if k.eq_ignore_ascii_case("oblique") => {
            let a = angle_of(a)?;
            Some(StyleRange::Oblique(a, a))
        }
        [Token::Ident(k), a, b] if k.eq_ignore_ascii_case("oblique") => {
            let (a, b) = (angle_of(a)?, angle_of(b)?);
            Some(StyleRange::Oblique(a.min(b), a.max(b)))
        }
        _ => None,
    }
}

/// `font-feature-settings` (`normal` or a list of `<string> [<integer> |
/// on | off]?`).
pub fn parse_feature_settings(value: &str) -> Option<Vec<([u8; 4], u32)>> {
    let tokens = significant(value);
    if let [Token::Ident(k)] = tokens.as_slice() {
        return k.eq_ignore_ascii_case("normal").then(Vec::new);
    }
    split_commas(&tokens)
        .into_iter()
        .map(|part| {
            let (tag, v) = match part {
                [Token::String(t)] => (t, 1),
                [Token::String(t), Token::Number(n)] if n.integer && n.value >= 0.0 => {
                    (t, n.value as u32)
                }
                [Token::String(t), Token::Ident(k)] if k.eq_ignore_ascii_case("on") => (t, 1),
                [Token::String(t), Token::Ident(k)] if k.eq_ignore_ascii_case("off") => (t, 0),
                _ => return None,
            };
            let b = tag.as_bytes();
            if b.len() != 4 || !b.iter().all(|c| (0x20..=0x7e).contains(c)) {
                return None;
            }
            Some(([b[0], b[1], b[2], b[3]], v))
        })
        .collect()
}

/// `font-display`.
pub fn valid_display(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "auto" | "block" | "swap" | "fallback" | "optional"
    )
}

/// `ascent-override`, `descent-override`, `line-gap-override`: `normal`
/// or a non-negative percentage.
pub fn valid_metric_override(value: &str) -> bool {
    match significant(value).as_slice() {
        [Token::Ident(k)] => k.eq_ignore_ascii_case("normal"),
        [Token::Percentage(p)] => p.value >= 0.0,
        _ => false,
    }
}

/// `font-variation-settings`: `normal` or `<string> <number>` pairs.
pub fn valid_variation_settings(value: &str) -> bool {
    let tokens = significant(value);
    if let [Token::Ident(k)] = tokens.as_slice() {
        return k.eq_ignore_ascii_case("normal");
    }
    split_commas(&tokens).into_iter().all(
        |part| matches!(part, [Token::String(t), Token::Number(_)] if t.len() == 4 && t.is_ascii()),
    )
}

/// What the `font` shorthand says about face selection, for
/// `FontFaceSet.load()` and `check()` (CSS Font Loading 3 §2.4).
#[derive(Clone, Debug, PartialEq)]
pub struct FontQuery {
    pub families: Vec<String>,
    pub weight: f32,
    pub stretch: f32,
    pub style: vgfx::font::matching::Style,
}

/// Parses a `font` shorthand value (CSS Fonts 4 §2.8). A CSS-wide
/// keyword or an invalid value gives `None`; a system font keyword
/// selects `sans-serif` (D-15).
pub fn parse_font_shorthand(value: &str) -> Option<FontQuery> {
    use vgfx::font::matching::Style;
    let tokens = significant(value);
    let mut q = FontQuery {
        families: Vec::new(),
        weight: 400.0,
        stretch: 100.0,
        style: Style::Normal,
    };
    if let [Token::Ident(k)] = tokens.as_slice() {
        let k = k.to_ascii_lowercase();
        if matches!(
            k.as_str(),
            "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar"
        ) {
            q.families.push("sans-serif".into());
            return Some(q);
        }
        return None;
    }
    let is_size = |t: &Token| match t {
        Token::Dimension(n, unit) => {
            n.value >= 0.0
                && matches!(
                    unit.to_ascii_lowercase().as_str(),
                    "px" | "em"
                        | "rem"
                        | "ex"
                        | "rex"
                        | "ch"
                        | "rch"
                        | "ic"
                        | "ric"
                        | "cap"
                        | "rcap"
                        | "lh"
                        | "rlh"
                        | "cm"
                        | "mm"
                        | "q"
                        | "in"
                        | "pt"
                        | "pc"
                        | "vw"
                        | "vh"
                        | "vmin"
                        | "vmax"
                        | "vi"
                        | "vb"
                )
        }
        Token::Percentage(n) => n.value >= 0.0,
        Token::Number(n) => n.value == 0.0,
        Token::Ident(k) => matches!(
            k.to_ascii_lowercase().as_str(),
            "xx-small"
                | "x-small"
                | "small"
                | "medium"
                | "large"
                | "x-large"
                | "xx-large"
                | "xxx-large"
                | "larger"
                | "smaller"
        ),
        _ => false,
    };
    let mut i = 0;
    let (mut style, mut weight, mut variant, mut stretch) = (false, false, false, false);
    let mut count = 0;
    loop {
        let t = tokens.get(i)?;
        if is_size(t) {
            break;
        }
        count += 1;
        if count > 4 {
            return None;
        }
        match t {
            Token::Ident(k) => match k.to_ascii_lowercase().as_str() {
                "normal" => {}
                "italic" if !style => {
                    style = true;
                    q.style = Style::Italic;
                }
                "oblique" if !style => {
                    style = true;
                    q.style = Style::Oblique(14.0);
                    if let Some(a) = tokens.get(i + 1).and_then(angle_of) {
                        if !matches!(tokens.get(i + 1), Some(Token::Number(_))) {
                            q.style = Style::Oblique(a);
                            i += 1;
                        }
                    }
                }
                "small-caps" if !variant => variant = true,
                "bold" if !weight => {
                    weight = true;
                    q.weight = 700.0;
                }
                "bolder" | "lighter" if !weight => weight = true,
                k if !stretch => {
                    q.stretch = parse_stretch(k)?.0;
                    stretch = true;
                }
                _ => return None,
            },
            Token::Number(n) if !weight && (1.0..=1000.0).contains(&n.value) => {
                weight = true;
                q.weight = n.value as f32;
            }
            _ => return None,
        }
        i += 1;
    }
    i += 1;
    if tokens.get(i) == Some(&Token::Delim('/')) {
        match tokens.get(i + 1)? {
            Token::Ident(k) if k.eq_ignore_ascii_case("normal") => {}
            Token::Number(_) | Token::Dimension(..) | Token::Percentage(_) => {}
            _ => return None,
        }
        i += 2;
    }
    let rest = &tokens[i..];
    if rest.is_empty() {
        return None;
    }
    q.families = split_commas(rest)
        .into_iter()
        .map(|part| family_name(part, false))
        .collect::<Option<Vec<_>>>()?;
    Some(q)
}

/// The matching descriptor of a face from its descriptor values
/// (`None` for an invalid one).
pub fn descriptor(weight: &str, stretch: &str, style: &str) -> Option<Descriptor> {
    Some(Descriptor {
        weight: parse_weight(weight)?,
        stretch: parse_stretch(stretch)?,
        style: parse_style(style)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn src_lists() {
        let s = parse_src(
            "local(Ahem), url(a.woff2) format(\"woff2\"), url('b.eot') format(embedded-opentype), url(c.ttf) tech(features-opentype)",
        )
        .unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], FontSource::Local("Ahem".into()));
        assert_eq!(
            s[1],
            FontSource::Url {
                url: "a.woff2".into(),
                format: Some("woff2".into()),
                tech: vec![]
            }
        );
        assert!(matches!(&s[2], FontSource::Url { url, .. } if url == "c.ttf"));
        assert_eq!(parse_src("url(a) junk"), None);
        assert_eq!(parse_src("url(a) format(svg)"), None);
        assert_eq!(
            parse_src("local(\"Liberation Sans\")").unwrap(),
            vec![FontSource::Local("Liberation Sans".into())]
        );
    }

    #[test]
    fn unicode_ranges() {
        assert_eq!(parse_unicode_range("U+0-7F"), Some(vec![(0, 0x7f)]));
        assert_eq!(
            parse_unicode_range("u+4??, U+20"),
            Some(vec![(0x400, 0x4ff), (0x20, 0x20)])
        );
        assert_eq!(parse_unicode_range("U+110000"), None);
        assert_eq!(parse_unicode_range("U+30-20"), None);
        assert_eq!(parse_unicode_range("0-7F"), None);
    }

    #[test]
    fn weight_stretch_style() {
        assert_eq!(parse_weight("bold"), Some((700.0, 700.0)));
        assert_eq!(parse_weight("900 300"), Some((300.0, 900.0)));
        assert_eq!(parse_weight("0"), None);
        assert_eq!(parse_stretch("condensed"), Some((75.0, 75.0)));
        assert_eq!(parse_stretch("50% 200%"), Some((50.0, 200.0)));
        assert_eq!(
            parse_style("oblique 10deg 20deg"),
            Some(StyleRange::Oblique(10.0, 20.0))
        );
        assert_eq!(parse_style("italic"), Some(StyleRange::Italic));
        assert_eq!(parse_style("oblique 100deg"), None);
    }

    #[test]
    fn family_names() {
        assert_eq!(
            parse_family_name("Liberation  Sans"),
            Some("Liberation Sans".into())
        );
        assert_eq!(parse_family_name("'serif'"), Some("serif".into()));
        assert_eq!(parse_family_name("serif"), None);
        assert_eq!(parse_family_name("inherit"), None);
        assert_eq!(parse_family_name("a, b"), None);
        assert_eq!(
            parse_family_list("Ahem, sans-serif"),
            Some(vec!["Ahem".into(), "sans-serif".into()])
        );
    }

    #[test]
    fn font_shorthand() {
        use vgfx::font::matching::Style;
        let q = parse_font_shorthand("italic bold 10px/2 Ahem, serif").unwrap();
        assert_eq!(q.families, vec!["Ahem".to_string(), "serif".to_string()]);
        assert_eq!((q.weight, q.style), (700.0, Style::Italic));
        let q = parse_font_shorthand("condensed 300 12pt 'A B'").unwrap();
        assert_eq!((q.weight, q.stretch), (300.0, 75.0));
        assert_eq!(parse_font_shorthand("inherit"), None);
        assert_eq!(parse_font_shorthand("10px"), None);
        assert_eq!(parse_font_shorthand("Ahem"), None);
        assert_eq!(
            parse_font_shorthand("caption").unwrap().families,
            vec!["sans-serif".to_string()]
        );
    }

    #[test]
    fn feature_settings() {
        assert_eq!(parse_feature_settings("normal"), Some(vec![]));
        assert_eq!(
            parse_feature_settings("\"liga\" off, \"ss01\" 2, \"kern\""),
            Some(vec![(*b"liga", 0), (*b"ss01", 2), (*b"kern", 1)])
        );
        assert_eq!(parse_feature_settings("\"toolong\""), None);
    }
}
