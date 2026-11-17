//! UTS #46 (Unicode IDNA Compatibility Processing, Unicode 17.0.0) and
//! Punycode (RFC 3492), as the URL Standard's domain to ASCII and domain to
//! Unicode use them: CheckHyphens false, CheckBidi true, CheckJoiners true,
//! nontransitional processing, IgnoreInvalidPunycode false, and
//! UseSTD3ASCIIRules and VerifyDnsLength equal to beStrict.

use crate::generated::idna::{
    BIDI_AL, BIDI_AN, BIDI_BN, BIDI_CLASS, BIDI_CS, BIDI_EN, BIDI_ES, BIDI_ET, BIDI_L, BIDI_NSM,
    BIDI_ON, BIDI_R, JOINING_D, JOINING_L, JOINING_R, JOINING_T, JOINING_TYPE, MAPPING,
    MAPPING_TEXT, STATUS_DEVIATION, STATUS_IGNORED, STATUS_MAPPED, STATUS_VALID,
};
use vjs::unicode::{NormalizationForm, combining_class, general_category, normalize};

fn range_value(table: &[(u32, u32, u8)], c: u32) -> Option<u8> {
    let i = table.partition_point(|&(_, last, _)| last < c);
    table
        .get(i)
        .filter(|&&(first, _, _)| first <= c)
        .map(|&(_, _, v)| v)
}

/// IDNA Mapping Table status of `c`, and the replacement when `mapped`.
fn mapping(c: char) -> (u8, &'static str) {
    let c = c as u32;
    let i = MAPPING.partition_point(|&(_, last, _, _, _)| last < c);
    let (_, _, status, offset, len) = MAPPING[i];
    let offset = offset as usize;
    (status, &MAPPING_TEXT[offset..offset + usize::from(len)])
}

fn bidi_class(c: char) -> u8 {
    range_value(BIDI_CLASS, c as u32).unwrap_or(BIDI_L)
}

/// Joining_Type of `c`; `None` for U (Non_Joining).
fn joining_type(c: char) -> Option<u8> {
    range_value(JOINING_TYPE, c as u32)
}

fn is_mark(c: char) -> bool {
    general_category(c as u32).starts_with('M')
}

const VIRAMA: u8 = 9;

/// The result of UTS #46 processing: the labels after Convert/Validate and
/// whether an error was recorded.
struct Processed {
    labels: Vec<String>,
    error: bool,
}

/// UTS #46 §4 Processing.
fn process(domain: &str, use_std3_ascii_rules: bool) -> Processed {
    let mut error = false;
    // 1. Map.
    let mut mapped: Vec<u32> = Vec::with_capacity(domain.len());
    for c in domain.chars() {
        let (status, replacement) = mapping(c);
        match status {
            STATUS_IGNORED => {}
            STATUS_MAPPED => mapped.extend(replacement.chars().map(|r| r as u32)),
            // Disallowed code points stay; Convert/Validate rejects them.
            _ => mapped.push(c as u32),
        }
    }
    // 2. Normalize.
    let normalized: String = normalize(&mapped, NormalizationForm::Nfc)
        .into_iter()
        .map(|c| char::from_u32(c).unwrap_or('\u{FFFD}'))
        .collect();
    // 3. Break; 4. Convert/Validate.
    let mut labels = Vec::new();
    for label in normalized.split('.') {
        if let Some(rest) = label.strip_prefix("xn--") {
            if !label.is_ascii() {
                error = true;
                labels.push(label.to_string());
                continue;
            }
            let Some(decoded) = punycode_decode(rest) else {
                error = true;
                labels.push(label.to_string());
                continue;
            };
            let decoded: String = decoded.into_iter().collect();
            if decoded.is_empty() || decoded.is_ascii() {
                error = true;
            }
            if !valid_label(&decoded, use_std3_ascii_rules) {
                error = true;
            }
            labels.push(decoded);
        } else {
            if !valid_label(label, use_std3_ascii_rules) {
                error = true;
            }
            labels.push(label.to_string());
        }
    }
    // CheckBidi applies only to a Bidi domain name: one with an RTL label.
    let bidi_domain = labels.iter().any(|label| {
        label
            .chars()
            .any(|c| matches!(bidi_class(c), BIDI_R | BIDI_AL | BIDI_AN))
    });
    if bidi_domain && !labels.iter().all(|label| bidi_rule(label)) {
        error = true;
    }
    Processed { labels, error }
}

/// UTS #46 §4.1 Validity Criteria (nontransitional, CheckHyphens false,
/// CheckJoiners true); CheckBidi is checked per domain in [`process`].
fn valid_label(label: &str, use_std3_ascii_rules: bool) -> bool {
    if label.is_empty() {
        return true;
    }
    let cps: Vec<u32> = label.chars().map(|c| c as u32).collect();
    if normalize(&cps, NormalizationForm::Nfc) != cps {
        return false;
    }
    if label.starts_with("xn--") {
        return false;
    }
    if label.contains('.') {
        return false;
    }
    if label.chars().next().is_some_and(is_mark) {
        return false;
    }
    for c in label.chars() {
        let (status, _) = mapping(c);
        if status != STATUS_VALID && status != STATUS_DEVIATION {
            return false;
        }
        if use_std3_ascii_rules && c.is_ascii() && !matches!(c, 'a'..='z' | '0'..='9' | '-') {
            return false;
        }
    }
    check_joiners(label)
}

/// RFC 5892 Appendix A.1 and A.2 (ContextJ).
fn check_joiners(label: &str) -> bool {
    let chars: Vec<char> = label.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c != '\u{200C}' && c != '\u{200D}' {
            continue;
        }
        if i > 0 && combining_class(chars[i - 1] as u32) == VIRAMA {
            continue;
        }
        if c == '\u{200D}' {
            return false;
        }
        // (Joining_Type:{L,D})(Joining_Type:T)*‌(Joining_Type:T)*(Joining_Type:{R,D})
        let before = chars[..i]
            .iter()
            .rev()
            .map(|&b| joining_type(b))
            .find(|&t| t != Some(JOINING_T));
        let after = chars[i + 1..]
            .iter()
            .map(|&a| joining_type(a))
            .find(|&t| t != Some(JOINING_T));
        let before_ok = matches!(before, Some(Some(t)) if t == JOINING_L || t == JOINING_D);
        let after_ok = matches!(after, Some(Some(t)) if t == JOINING_R || t == JOINING_D);
        if !(before_ok && after_ok) {
            return false;
        }
    }
    true
}

/// The six conditions of RFC 5893 §2 (the Bidi Rule) on one label.
fn bidi_rule(label: &str) -> bool {
    let classes: Vec<u8> = label.chars().map(bidi_class).collect();
    let Some(&first) = classes.first() else {
        return true;
    };
    // The last class that is not NSM.
    let last = classes.iter().rev().find(|&&c| c != BIDI_NSM).copied();
    if first == BIDI_R || first == BIDI_AL {
        // Conditions 2, 3 and 4.
        let allowed = [
            BIDI_R, BIDI_AL, BIDI_AN, BIDI_EN, BIDI_ES, BIDI_CS, BIDI_ET, BIDI_ON, BIDI_BN,
            BIDI_NSM,
        ];
        if !classes.iter().all(|c| allowed.contains(c)) {
            return false;
        }
        if !matches!(last, Some(c) if c == BIDI_R || c == BIDI_AL || c == BIDI_EN || c == BIDI_AN) {
            return false;
        }
        !(classes.contains(&BIDI_EN) && classes.contains(&BIDI_AN))
    } else if first == BIDI_L {
        // Conditions 5 and 6.
        let allowed = [
            BIDI_L, BIDI_EN, BIDI_ES, BIDI_CS, BIDI_ET, BIDI_ON, BIDI_BN, BIDI_NSM,
        ];
        if !classes.iter().all(|c| allowed.contains(c)) {
            return false;
        }
        matches!(last, Some(c) if c == BIDI_L || c == BIDI_EN)
    } else {
        // Condition 1.
        false
    }
}

/// UTS #46 ToASCII with the URL Standard's flags; `None` on error.
pub fn to_ascii(domain: &str, be_strict: bool) -> Option<String> {
    let processed = process(domain, be_strict);
    if processed.error {
        return None;
    }
    let mut labels = Vec::with_capacity(processed.labels.len());
    for label in processed.labels {
        if label.is_ascii() {
            labels.push(label);
        } else {
            labels.push(format!(
                "xn--{}",
                punycode_encode(&label.chars().collect::<Vec<_>>())?
            ));
        }
    }
    let result = labels.join(".");
    if be_strict {
        // VerifyDnsLength: the root label may be absent but no other empty
        // label, at most 253 octets and 63 per label.
        let trimmed = result.strip_suffix('.').unwrap_or(&result);
        if trimmed.is_empty()
            || trimmed.len() > 253
            || trimmed.split('.').any(|l| l.is_empty() || l.len() > 63)
        {
            return None;
        }
    }
    Some(result)
}

/// UTS #46 ToUnicode with the URL Standard's flags: the Unicode form of
/// `domain` and whether processing recorded an error.
pub fn to_unicode(domain: &str, be_strict: bool) -> (String, bool) {
    let processed = process(domain, be_strict);
    (processed.labels.join("."), processed.error)
}

// ----- Punycode (RFC 3492) -----

const BASE: u32 = 36;
const T_MIN: u32 = 1;
const T_MAX: u32 = 26;
const SKEW: u32 = 38;
const DAMP: u32 = 700;
const INITIAL_BIAS: u32 = 72;
const INITIAL_N: u32 = 128;

fn adapt(delta: u32, num_points: u32, first_time: bool) -> u32 {
    let mut delta = if first_time { delta / DAMP } else { delta / 2 };
    delta += delta / num_points;
    let mut k = 0;
    while delta > ((BASE - T_MIN) * T_MAX) / 2 {
        delta /= BASE - T_MIN;
        k += BASE;
    }
    k + (BASE - T_MIN + 1) * delta / (delta + SKEW)
}

fn threshold(k: u32, bias: u32) -> u32 {
    if k <= bias {
        T_MIN
    } else if k >= bias + T_MAX {
        T_MAX
    } else {
        k - bias
    }
}

fn digit_value(c: char) -> Option<u32> {
    match c {
        'a'..='z' => Some(c as u32 - 'a' as u32),
        'A'..='Z' => Some(c as u32 - 'A' as u32),
        '0'..='9' => Some(c as u32 - '0' as u32 + 26),
        _ => None,
    }
}

fn digit_char(d: u32) -> char {
    if d < 26 {
        (b'a' + d as u8) as char
    } else {
        (b'0' + (d - 26) as u8) as char
    }
}

/// Decode a Punycode string (without the `xn--` prefix).
pub fn punycode_decode(input: &str) -> Option<Vec<char>> {
    let (basic, extended) = match input.rfind('-') {
        Some(i) => (&input[..i], &input[i + 1..]),
        None => ("", input),
    };
    if !basic.is_ascii() {
        return None;
    }
    let mut output: Vec<char> = basic.chars().collect();
    let mut n = INITIAL_N;
    let mut i: u32 = 0;
    let mut bias = INITIAL_BIAS;
    let mut chars = extended.chars().peekable();
    while chars.peek().is_some() {
        let old_i = i;
        let mut w: u32 = 1;
        let mut k = BASE;
        loop {
            let digit = digit_value(chars.next()?)?;
            i = i.checked_add(digit.checked_mul(w)?)?;
            let t = threshold(k, bias);
            if digit < t {
                break;
            }
            w = w.checked_mul(BASE - t)?;
            k += BASE;
        }
        let len = output.len() as u32 + 1;
        bias = adapt(i - old_i, len, old_i == 0);
        n = n.checked_add(i / len)?;
        i %= len;
        let c = char::from_u32(n)?;
        output.insert(i as usize, c);
        i += 1;
    }
    Some(output)
}

/// Encode code points as Punycode (without the `xn--` prefix); `None` on
/// overflow.
pub fn punycode_encode(input: &[char]) -> Option<String> {
    let mut output: String = input.iter().filter(|c| c.is_ascii()).collect();
    let basic = output.len() as u32;
    let mut handled = basic;
    if basic > 0 {
        output.push('-');
    }
    let mut n = INITIAL_N;
    let mut delta: u32 = 0;
    let mut bias = INITIAL_BIAS;
    let total = input.len() as u32;
    while handled < total {
        let m = input.iter().map(|&c| c as u32).filter(|&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(handled + 1)?)?;
        n = m;
        for &c in input {
            let c = c as u32;
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = threshold(k, bias);
                    if q < t {
                        break;
                    }
                    output.push(digit_char(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                output.push(digit_char(q));
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta = delta.checked_add(1)?;
        n = n.checked_add(1)?;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punycode_round_trips_rfc_samples() {
        let bucher: Vec<char> = "bücher".chars().collect();
        assert_eq!(punycode_encode(&bucher).as_deref(), Some("bcher-kva"));
        assert_eq!(punycode_decode("bcher-kva"), Some(bucher));
        assert_eq!(
            punycode_decode("ls8h").map(|v| v.into_iter().collect::<String>()),
            Some("\u{1F4A9}".into())
        );
    }

    #[test]
    fn deviations_are_kept() {
        assert_eq!(to_ascii("faß.de", false).as_deref(), Some("xn--fa-hia.de"));
        assert_eq!(to_ascii("FAẞ.DE", false).as_deref(), Some("xn--fa-hia.de"));
    }

    #[test]
    fn invalid_punycode_and_ascii_results_fail() {
        assert_eq!(to_ascii("xn--", false), None);
        assert_eq!(to_ascii("xn--a", false), None);
        assert_eq!(to_ascii("xn--ls8h", false).as_deref(), Some("xn--ls8h"));
    }

    #[test]
    fn joiners_need_context() {
        assert_eq!(to_ascii("a\u{200D}b", false), None);
        // ZWJ after a virama (U+094D) is fine.
        assert!(to_ascii("\u{915}\u{94D}\u{200D}", false).is_some());
    }

    #[test]
    fn bidi_rule_rejects_mixed_labels() {
        // A Hebrew label that starts with a digit breaks condition 1.
        assert_eq!(to_ascii("1\u{5D0}", false), None);
        assert!(to_ascii("\u{5D0}\u{5D1}", false).is_some());
    }
}
