//! Function properties of the global object (ECMA-262 §19.2): `isFinite`,
//! `isNaN`, `parseFloat`, `parseInt` and the URI functions. `eval` is
//! created with the interpreter's intrinsics; `globalThis` and the value
//! properties by SetDefaultGlobalBindings.

use crate::agent::{Agent, ErrorKind};
use crate::bigint::BigInt;
use crate::builtins::{builtin_function, set_intrinsic};
use crate::completion::JsResult;
use crate::function::NativeCall;
use crate::gc::Realm;
use crate::number::trim_str_whitespace;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::Value;

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let functions: [(Intrinsic, &str, crate::function::NativeFn, u32); 8] = [
        (Intrinsic::IsFinite, "isFinite", is_finite, 1),
        (Intrinsic::IsNaN, "isNaN", is_nan, 1),
        (Intrinsic::ParseFloat, "parseFloat", parse_float, 1),
        (Intrinsic::ParseInt, "parseInt", parse_int, 2),
        (Intrinsic::EncodeUri, "encodeURI", encode_uri, 1),
        (
            Intrinsic::EncodeUriComponent,
            "encodeURIComponent",
            encode_uri_component,
            1,
        ),
        (Intrinsic::DecodeUri, "decodeURI", decode_uri, 1),
        (
            Intrinsic::DecodeUriComponent,
            "decodeURIComponent",
            decode_uri_component,
            1,
        ),
    ];
    for (which, name, f, length) in functions {
        let func = builtin_function(agent, realm, name, f, length);
        set_intrinsic(agent, realm, which, func);
    }
}

fn is_finite(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let n = ops::to_number(agent, call.arg(0))?;
    Ok(Value::Boolean(n.is_finite()))
}

fn is_nan(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let n = ops::to_number(agent, call.arg(0))?;
    Ok(Value::Boolean(n.is_nan()))
}

/// The input string of `parseFloat`/`parseInt` without leading white
/// space, as code units.
fn trimmed_input(agent: &mut Agent, v: Value) -> JsResult<Vec<u16>> {
    let s = ops::to_string(agent, v)?;
    let units = agent.heap.string_units(s);
    let trimmed = trim_str_whitespace(units);
    // Only the leading white space matters: the scans below stop at the
    // first unit that cannot continue the literal.
    Ok(trimmed.to_vec())
}

/// The longest prefix of `units` that is a StrDecimalLiteral, as ASCII.
pub(crate) fn str_decimal_prefix(units: &[u16]) -> Option<String> {
    let ascii = |i: usize| units.get(i).and_then(|&u| u8::try_from(u).ok());
    let mut i = 0;
    let mut out = String::new();
    if let Some(c @ (b'+' | b'-')) = ascii(0) {
        out.push(char::from(c));
        i = 1;
    }
    let infinity = crate::string::utf16("Infinity");
    if units[i..].starts_with(&infinity) {
        out.push_str("inf");
        return Some(out);
    }
    let digits_from = |mut j: usize, out: &mut String| -> usize {
        while let Some(c @ b'0'..=b'9') = ascii(j) {
            out.push(char::from(c));
            j += 1;
        }
        j
    };
    let int_end = digits_from(i, &mut out);
    let mut any_digits = int_end > i;
    i = int_end;
    if ascii(i) == Some(b'.') {
        let mut frac = String::from(".");
        let frac_end = digits_from(i + 1, &mut frac);
        if frac_end > i + 1 || any_digits {
            any_digits |= frac_end > i + 1;
            out.push_str(&frac);
            i = frac_end;
        }
    }
    if !any_digits {
        return None;
    }
    if let Some(b'e' | b'E') = ascii(i) {
        let mut exp = String::from("e");
        let mut j = i + 1;
        if let Some(c @ (b'+' | b'-')) = ascii(j) {
            exp.push(char::from(c));
            j += 1;
        }
        let exp_end = digits_from(j, &mut exp);
        if exp_end > j {
            out.push_str(&exp);
        }
    }
    Some(out)
}

/// `parseFloat(string)` (§19.2.4).
fn parse_float(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let units = trimmed_input(agent, call.arg(0))?;
    let n = match str_decimal_prefix(&units) {
        // `inf` stands for Infinity; "-0" gives -0.
        Some(text) => match text.strip_suffix("inf") {
            Some(sign) => {
                if sign == "-" {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }
            }
            None => crate::numconv::parse_decimal(text.as_bytes()).unwrap_or(f64::NAN),
        },
        None => f64::NAN,
    };
    Ok(Value::Number(n))
}

/// `parseInt(string, radix)` (§19.2.5).
fn parse_int(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let units = trimmed_input(agent, call.arg(0))?;
    let mut r = ops::to_int32(agent, call.arg(1))?;
    let mut s = &units[..];
    let mut sign = 1.0;
    if let Some(&first) = s.first() {
        if first == u16::from(b'-') {
            sign = -1.0;
        }
        if first == u16::from(b'-') || first == u16::from(b'+') {
            s = &s[1..];
        }
    }
    let mut strip_prefix = true;
    if r != 0 {
        if !(2..=36).contains(&r) {
            return Ok(Value::Number(f64::NAN));
        }
        if r != 16 {
            strip_prefix = false;
        }
    } else {
        r = 10;
    }
    if strip_prefix
        && s.len() >= 2
        && s[0] == u16::from(b'0')
        && (s[1] == u16::from(b'x') || s[1] == u16::from(b'X'))
    {
        s = &s[2..];
        r = 16;
    }
    let radix = r as u32;
    let end = s
        .iter()
        .position(|&u| {
            char::from_u32(u32::from(u))
                .and_then(|c| c.to_digit(radix))
                .is_none()
        })
        .unwrap_or(s.len());
    if end == 0 {
        return Ok(Value::Number(f64::NAN));
    }
    let digits: Vec<u8> = s[..end].iter().map(|&u| u as u8).collect();
    let magnitude = if radix == 10 {
        // Correctly rounded, as the specification allows for radix 10.
        crate::numconv::parse_decimal(&digits).unwrap_or(f64::NAN)
    } else {
        BigInt::parse_radix(&digits, radix).map_or(f64::NAN, |b| b.to_f64())
    };
    Ok(Value::Number(sign * magnitude))
}

// ----- URI handling (§19.2.6) -----

const URI_RESERVED: &[u8] = b";/?:@&=+$,";
const URI_MARK: &[u8] = b"-_.!~*'()";

fn is_unreserved(c: u16) -> bool {
    u8::try_from(c).is_ok_and(|b| b.is_ascii_alphanumeric() || URI_MARK.contains(&b))
}

fn uri_error<T>(agent: &mut Agent, message: &str) -> JsResult<T> {
    Err(agent.throw_error(ErrorKind::Uri, message))
}

/// Encode(string, extraUnescaped).
fn encode(agent: &mut Agent, v: Value, extra: &[u8]) -> JsResult<Value> {
    let s = ops::to_string(agent, v)?;
    let units = agent.heap.string_units(s).to_vec();
    let mut out: Vec<u16> = Vec::with_capacity(units.len());
    let mut k = 0;
    while k < units.len() {
        let c = units[k];
        if is_unreserved(c) || u8::try_from(c).is_ok_and(|b| extra.contains(&b)) {
            out.push(c);
            k += 1;
            continue;
        }
        // CodePointAt: a lone surrogate is an error.
        let cp = if (0xD800..0xDC00).contains(&c) {
            match units.get(k + 1) {
                Some(&next) if (0xDC00..0xE000).contains(&next) => {
                    k += 1;
                    0x10000 + ((u32::from(c) - 0xD800) << 10) + (u32::from(next) - 0xDC00)
                }
                _ => return uri_error(agent, "URI malformed"),
            }
        } else if (0xDC00..0xE000).contains(&c) {
            return uri_error(agent, "URI malformed");
        } else {
            u32::from(c)
        };
        k += 1;
        let ch = char::from_u32(cp).expect("a valid scalar value");
        let mut buf = [0u8; 4];
        for &byte in ch.encode_utf8(&mut buf).as_bytes() {
            for unit in format!("%{byte:02X}").encode_utf16() {
                out.push(unit);
            }
        }
    }
    Ok(Value::String(agent.heap.alloc_string_owned(out)))
}

fn hex_digit(u: u16) -> Option<u8> {
    char::from_u32(u32::from(u))
        .and_then(|c| c.to_digit(16))
        .map(|d| d as u8)
}

/// The byte of the escape `%XY` at `k`, if there is one.
fn escape_at(units: &[u16], k: usize) -> Option<u8> {
    if units.get(k) != Some(&u16::from(b'%')) {
        return None;
    }
    let hi = hex_digit(*units.get(k + 1)?)?;
    let lo = hex_digit(*units.get(k + 2)?)?;
    Some(hi << 4 | lo)
}

/// Decode(string, preserveEscapeSet).
fn decode(agent: &mut Agent, v: Value, preserve: &[u8]) -> JsResult<Value> {
    let s = ops::to_string(agent, v)?;
    let units = agent.heap.string_units(s).to_vec();
    let mut out: Vec<u16> = Vec::with_capacity(units.len());
    let mut k = 0;
    while k < units.len() {
        let c = units[k];
        if c != u16::from(b'%') {
            out.push(c);
            k += 1;
            continue;
        }
        let start = k;
        let Some(b) = escape_at(&units, k) else {
            return uri_error(agent, "URI malformed");
        };
        k += 3;
        if b < 0x80 {
            if preserve.contains(&b) {
                out.extend_from_slice(&units[start..k]);
            } else {
                out.push(u16::from(b));
            }
            continue;
        }
        let n = b.leading_ones() as usize;
        if n == 1 || n > 4 {
            return uri_error(agent, "URI malformed");
        }
        let mut octets = vec![b];
        for _ in 1..n {
            match escape_at(&units, k) {
                Some(cont) if cont & 0xC0 == 0x80 => octets.push(cont),
                _ => return uri_error(agent, "URI malformed"),
            }
            k += 3;
        }
        // Rust's UTF-8 validation rejects overlong forms, surrogates and
        // code points past U+10FFFF, as the specification requires.
        let Ok(text) = std::str::from_utf8(&octets) else {
            return uri_error(agent, "URI malformed");
        };
        out.extend(text.encode_utf16());
    }
    Ok(Value::String(agent.heap.alloc_string_owned(out)))
}

fn encode_uri(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    encode(agent, call.arg(0), b";/?:@&=+$,#")
}

fn encode_uri_component(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    encode(agent, call.arg(0), b"")
}

fn decode_uri(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let mut preserve = URI_RESERVED.to_vec();
    preserve.push(b'#');
    decode(agent, call.arg(0), &preserve)
}

fn decode_uri_component(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    decode(agent, call.arg(0), b"")
}

#[cfg(test)]
mod tests {
    use super::str_decimal_prefix;
    use crate::string::utf16;

    #[test]
    fn decimal_prefixes() {
        let p = |s: &str| str_decimal_prefix(&utf16(s));
        assert_eq!(p("1.5e3xyz").as_deref(), Some("1.5e3"));
        assert_eq!(p("-.5").as_deref(), Some("-.5"));
        assert_eq!(p("5.").as_deref(), Some("5."));
        assert_eq!(p("1e").as_deref(), Some("1"));
        assert_eq!(p("Infinityx").as_deref(), Some("inf"));
        assert_eq!(p("-Infinity").as_deref(), Some("-inf"));
        assert_eq!(p("."), None);
        assert_eq!(p("e5"), None);
        assert_eq!(p("0x10").as_deref(), Some("0"));
    }
}
