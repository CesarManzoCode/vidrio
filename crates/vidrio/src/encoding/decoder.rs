//! Streaming decoders (Encoding §8 to §14). Each decoder is the handler of
//! the standard, one byte at a time, with "restore to ioQueue" kept in a
//! small stack the driver reads before the next input byte.

use super::Encoding;
use super::index;
use crate::generated::encoding_single_byte;

/// Error mode of a decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderErrorMode {
    /// Each error becomes U+FFFD.
    Replacement,
    /// The first error stops decoding.
    Fatal,
}

/// What the decoder does with a byte order mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BomHandling {
    /// A UTF-8 or UTF-16 byte order mark picks the encoding and is dropped
    /// (the decode hook).
    Sniff,
    /// A leading U+FEFF is dropped when the encoding is UTF-8 or UTF-16BE/LE
    /// (TextDecoder without `ignoreBOM`).
    Strip,
    /// The input is decoded as is (TextDecoder with `ignoreBOM`).
    Keep,
}

/// A decoding error in fatal mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeError;

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the encoded data was not valid")
    }
}

impl std::error::Error for DecodeError {}

/// The result of one run of a handler.
enum Step {
    Continue,
    Finished,
    Error,
    One(u32),
    Two(u32, u32),
}

/// Bytes restored to the input, read before the next input byte.
struct Restored {
    stack: [u8; 4],
    len: usize,
}

impl Restored {
    fn new() -> Self {
        Restored {
            stack: [0; 4],
            len: 0,
        }
    }

    /// Restore `bytes` so that `bytes[0]` is read next.
    fn restore(&mut self, bytes: &[u8]) {
        for &b in bytes.iter().rev() {
            self.stack[self.len] = b;
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            None
        } else {
            self.len -= 1;
            Some(self.stack[self.len])
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JpState {
    Ascii,
    Roman,
    Katakana,
    LeadByte,
    TrailByte,
    EscapeStart,
    Escape,
}

#[derive(Clone, Debug)]
enum State {
    Utf8 {
        code_point: u32,
        seen: u8,
        needed: u8,
        lower: u8,
        upper: u8,
    },
    SingleByte(&'static [u16; 128]),
    Gb18030 {
        first: u8,
        second: u8,
        third: u8,
    },
    Big5 {
        lead: u8,
    },
    EucJp {
        jis0212: bool,
        lead: u8,
    },
    Iso2022Jp {
        state: JpState,
        output_state: JpState,
        lead: u8,
        output: bool,
    },
    ShiftJis {
        lead: u8,
    },
    EucKr {
        lead: u8,
    },
    Replacement {
        error_returned: bool,
    },
    Utf16 {
        big_endian: bool,
        lead_byte: Option<u8>,
        lead_surrogate: Option<u16>,
    },
    XUserDefined,
}

impl State {
    fn new(encoding: Encoding) -> State {
        if let Some(table) = encoding_single_byte::index(encoding) {
            return State::SingleByte(table);
        }
        match encoding {
            Encoding::Utf8 => State::Utf8 {
                code_point: 0,
                seen: 0,
                needed: 0,
                lower: 0x80,
                upper: 0xBF,
            },
            Encoding::Gbk | Encoding::Gb18030 => State::Gb18030 {
                first: 0,
                second: 0,
                third: 0,
            },
            Encoding::Big5 => State::Big5 { lead: 0 },
            Encoding::EucJp => State::EucJp {
                jis0212: false,
                lead: 0,
            },
            Encoding::Iso2022Jp => State::Iso2022Jp {
                state: JpState::Ascii,
                output_state: JpState::Ascii,
                lead: 0,
                output: false,
            },
            Encoding::ShiftJis => State::ShiftJis { lead: 0 },
            Encoding::EucKr => State::EucKr { lead: 0 },
            Encoding::Replacement => State::Replacement {
                error_returned: false,
            },
            Encoding::Utf16Be | Encoding::Utf16Le => State::Utf16 {
                big_endian: encoding == Encoding::Utf16Be,
                lead_byte: None,
                lead_surrogate: None,
            },
            Encoding::XUserDefined => State::XUserDefined,
            _ => unreachable!("single-byte encodings are handled above"),
        }
    }

    fn handle(&mut self, byte: Option<u8>, queue: &mut Restored) -> Step {
        match self {
            State::Utf8 {
                code_point,
                seen,
                needed,
                lower,
                upper,
            } => {
                let Some(byte) = byte else {
                    if *needed != 0 {
                        *needed = 0;
                        return Step::Error;
                    }
                    return Step::Finished;
                };
                if *needed == 0 {
                    match byte {
                        0x00..=0x7F => return Step::One(byte.into()),
                        0xC2..=0xDF => {
                            *needed = 1;
                            *code_point = u32::from(byte & 0x1F);
                        }
                        0xE0..=0xEF => {
                            if byte == 0xE0 {
                                *lower = 0xA0;
                            }
                            if byte == 0xED {
                                *upper = 0x9F;
                            }
                            *needed = 2;
                            *code_point = u32::from(byte & 0xF);
                        }
                        0xF0..=0xF4 => {
                            if byte == 0xF0 {
                                *lower = 0x90;
                            }
                            if byte == 0xF4 {
                                *upper = 0x8F;
                            }
                            *needed = 3;
                            *code_point = u32::from(byte & 0x7);
                        }
                        _ => return Step::Error,
                    }
                    return Step::Continue;
                }
                if byte < *lower || byte > *upper {
                    *code_point = 0;
                    *needed = 0;
                    *seen = 0;
                    *lower = 0x80;
                    *upper = 0xBF;
                    queue.restore(&[byte]);
                    return Step::Error;
                }
                *lower = 0x80;
                *upper = 0xBF;
                *code_point = (*code_point << 6) | u32::from(byte & 0x3F);
                *seen += 1;
                if *seen != *needed {
                    return Step::Continue;
                }
                let c = *code_point;
                *code_point = 0;
                *needed = 0;
                *seen = 0;
                Step::One(c)
            }
            State::SingleByte(table) => match byte {
                None => Step::Finished,
                Some(b) if b < 0x80 => Step::One(b.into()),
                Some(b) => match table[usize::from(b - 0x80)] {
                    0 => Step::Error,
                    c => Step::One(c.into()),
                },
            },
            State::Gb18030 {
                first,
                second,
                third,
            } => {
                let Some(byte) = byte else {
                    if *first == 0 && *second == 0 && *third == 0 {
                        return Step::Finished;
                    }
                    *first = 0;
                    *second = 0;
                    *third = 0;
                    return Step::Error;
                };
                if *third != 0 {
                    if !(0x30..=0x39).contains(&byte) {
                        queue.restore(&[*second, *third, byte]);
                        *first = 0;
                        *second = 0;
                        *third = 0;
                        return Step::Error;
                    }
                    let pointer = (u32::from(*first) - 0x81) * (10 * 126 * 10)
                        + (u32::from(*second) - 0x30) * (10 * 126)
                        + (u32::from(*third) - 0x81) * 10
                        + u32::from(byte)
                        - 0x30;
                    *first = 0;
                    *second = 0;
                    *third = 0;
                    return match index::gb18030_ranges_code_point(pointer) {
                        Some(c) => Step::One(c),
                        None => Step::Error,
                    };
                }
                if *second != 0 {
                    if (0x81..=0xFE).contains(&byte) {
                        *third = byte;
                        return Step::Continue;
                    }
                    queue.restore(&[*second, byte]);
                    *first = 0;
                    *second = 0;
                    return Step::Error;
                }
                if *first != 0 {
                    if (0x30..=0x39).contains(&byte) {
                        *second = byte;
                        return Step::Continue;
                    }
                    let lead = usize::from(*first);
                    *first = 0;
                    let offset = if byte < 0x7F { 0x40 } else { 0x41 };
                    let pointer = if matches!(byte, 0x40..=0x7E | 0x80..=0xFE) {
                        Some((lead - 0x81) * 190 + (usize::from(byte) - offset))
                    } else {
                        None
                    };
                    if let Some(c) = pointer.and_then(index::gb18030_code_point) {
                        return Step::One(c);
                    }
                    if byte < 0x80 {
                        queue.restore(&[byte]);
                    }
                    return Step::Error;
                }
                match byte {
                    0x00..=0x7F => Step::One(byte.into()),
                    0x80 => Step::One(0x20AC),
                    0x81..=0xFE => {
                        *first = byte;
                        Step::Continue
                    }
                    _ => Step::Error,
                }
            }
            State::Big5 { lead } => {
                let Some(byte) = byte else {
                    if *lead != 0 {
                        *lead = 0;
                        return Step::Error;
                    }
                    return Step::Finished;
                };
                if *lead != 0 {
                    let leading = usize::from(*lead);
                    *lead = 0;
                    let offset = if byte < 0x7F { 0x40 } else { 0x62 };
                    let pointer = if matches!(byte, 0x40..=0x7E | 0xA1..=0xFE) {
                        Some((leading - 0x81) * 157 + (usize::from(byte) - offset))
                    } else {
                        None
                    };
                    match pointer {
                        Some(1133) => return Step::Two(0x00CA, 0x0304),
                        Some(1135) => return Step::Two(0x00CA, 0x030C),
                        Some(1164) => return Step::Two(0x00EA, 0x0304),
                        Some(1166) => return Step::Two(0x00EA, 0x030C),
                        _ => {}
                    }
                    if let Some(c) = pointer.and_then(index::big5_code_point) {
                        return Step::One(c);
                    }
                    if byte < 0x80 {
                        queue.restore(&[byte]);
                    }
                    return Step::Error;
                }
                match byte {
                    0x00..=0x7F => Step::One(byte.into()),
                    0x81..=0xFE => {
                        *lead = byte;
                        Step::Continue
                    }
                    _ => Step::Error,
                }
            }
            State::EucJp { jis0212, lead } => {
                let Some(byte) = byte else {
                    if *lead != 0 {
                        *lead = 0;
                        return Step::Error;
                    }
                    return Step::Finished;
                };
                if *lead == 0x8E && (0xA1..=0xDF).contains(&byte) {
                    *lead = 0;
                    return Step::One(0xFF61 - 0xA1 + u32::from(byte));
                }
                if *lead == 0x8F && (0xA1..=0xFE).contains(&byte) {
                    *jis0212 = true;
                    *lead = byte;
                    return Step::Continue;
                }
                if *lead != 0 {
                    let leading = *lead;
                    *lead = 0;
                    let mut code_point = None;
                    if (0xA1..=0xFE).contains(&leading) && (0xA1..=0xFE).contains(&byte) {
                        let pointer = (usize::from(leading) - 0xA1) * 94 + usize::from(byte) - 0xA1;
                        code_point = if *jis0212 {
                            index::jis0212_code_point(pointer)
                        } else {
                            index::jis0208_code_point(pointer)
                        };
                    }
                    *jis0212 = false;
                    if let Some(c) = code_point {
                        return Step::One(c);
                    }
                    if byte < 0x80 {
                        queue.restore(&[byte]);
                    }
                    return Step::Error;
                }
                match byte {
                    0x00..=0x7F => Step::One(byte.into()),
                    0x8E | 0x8F | 0xA1..=0xFE => {
                        *lead = byte;
                        Step::Continue
                    }
                    _ => Step::Error,
                }
            }
            State::Iso2022Jp {
                state,
                output_state,
                lead,
                output,
            } => match *state {
                JpState::Ascii | JpState::Roman | JpState::Katakana | JpState::LeadByte => {
                    let Some(byte) = byte else {
                        return Step::Finished;
                    };
                    if byte == 0x1B {
                        *state = JpState::EscapeStart;
                        return Step::Continue;
                    }
                    *output = false;
                    match *state {
                        JpState::Ascii if byte <= 0x7F && byte != 0x0E && byte != 0x0F => {
                            Step::One(byte.into())
                        }
                        JpState::Roman if byte == 0x5C => Step::One(0xA5),
                        JpState::Roman if byte == 0x7E => Step::One(0x203E),
                        JpState::Roman if byte <= 0x7F && byte != 0x0E && byte != 0x0F => {
                            Step::One(byte.into())
                        }
                        JpState::Katakana if (0x21..=0x5F).contains(&byte) => {
                            Step::One(0xFF61 - 0x21 + u32::from(byte))
                        }
                        JpState::LeadByte if (0x21..=0x7E).contains(&byte) => {
                            *lead = byte;
                            *state = JpState::TrailByte;
                            Step::Continue
                        }
                        _ => Step::Error,
                    }
                }
                JpState::TrailByte => match byte {
                    Some(0x1B) => {
                        *state = JpState::EscapeStart;
                        Step::Error
                    }
                    Some(b @ 0x21..=0x7E) => {
                        *state = JpState::LeadByte;
                        let pointer = (usize::from(*lead) - 0x21) * 94 + usize::from(b) - 0x21;
                        match index::jis0208_code_point(pointer) {
                            Some(c) => Step::One(c),
                            None => Step::Error,
                        }
                    }
                    _ => {
                        *state = JpState::LeadByte;
                        Step::Error
                    }
                },
                JpState::EscapeStart => {
                    if let Some(b @ (0x24 | 0x28)) = byte {
                        *lead = b;
                        *state = JpState::Escape;
                        return Step::Continue;
                    }
                    if let Some(b) = byte {
                        queue.restore(&[b]);
                    }
                    *output = false;
                    *state = *output_state;
                    Step::Error
                }
                JpState::Escape => {
                    let leading = *lead;
                    *lead = 0;
                    let next = match (leading, byte) {
                        (0x28, Some(0x42)) => Some(JpState::Ascii),
                        (0x28, Some(0x4A)) => Some(JpState::Roman),
                        (0x28, Some(0x49)) => Some(JpState::Katakana),
                        (0x24, Some(0x40 | 0x42)) => Some(JpState::LeadByte),
                        _ => None,
                    };
                    if let Some(next) = next {
                        *state = next;
                        *output_state = next;
                        let was_output = *output;
                        *output = true;
                        return if was_output {
                            Step::Error
                        } else {
                            Step::Continue
                        };
                    }
                    match byte {
                        None => queue.restore(&[leading]),
                        Some(b) => queue.restore(&[leading, b]),
                    }
                    *output = false;
                    *state = *output_state;
                    Step::Error
                }
            },
            State::ShiftJis { lead } => {
                let Some(byte) = byte else {
                    if *lead != 0 {
                        *lead = 0;
                        return Step::Error;
                    }
                    return Step::Finished;
                };
                if *lead != 0 {
                    let leading = usize::from(*lead);
                    *lead = 0;
                    let offset = if byte < 0x7F { 0x40 } else { 0x41 };
                    let lead_offset = if leading < 0xA0 { 0x81 } else { 0xC1 };
                    let pointer = if matches!(byte, 0x40..=0x7E | 0x80..=0xFC) {
                        Some((leading - lead_offset) * 188 + usize::from(byte) - offset)
                    } else {
                        None
                    };
                    if let Some(p @ 8836..=10715) = pointer {
                        return Step::One(0xE000 - 8836 + p as u32);
                    }
                    if let Some(c) = pointer.and_then(index::jis0208_code_point) {
                        return Step::One(c);
                    }
                    if byte < 0x80 {
                        queue.restore(&[byte]);
                    }
                    return Step::Error;
                }
                match byte {
                    0x00..=0x80 => Step::One(byte.into()),
                    0xA1..=0xDF => Step::One(0xFF61 - 0xA1 + u32::from(byte)),
                    0x81..=0x9F | 0xE0..=0xFC => {
                        *lead = byte;
                        Step::Continue
                    }
                    _ => Step::Error,
                }
            }
            State::EucKr { lead } => {
                let Some(byte) = byte else {
                    if *lead != 0 {
                        *lead = 0;
                        return Step::Error;
                    }
                    return Step::Finished;
                };
                if *lead != 0 {
                    let leading = usize::from(*lead);
                    *lead = 0;
                    let pointer = if (0x41..=0xFE).contains(&byte) {
                        Some((leading - 0x81) * 190 + (usize::from(byte) - 0x41))
                    } else {
                        None
                    };
                    if let Some(c) = pointer.and_then(index::euc_kr_code_point) {
                        return Step::One(c);
                    }
                    if byte < 0x80 {
                        queue.restore(&[byte]);
                    }
                    return Step::Error;
                }
                match byte {
                    0x00..=0x7F => Step::One(byte.into()),
                    0x81..=0xFE => {
                        *lead = byte;
                        Step::Continue
                    }
                    _ => Step::Error,
                }
            }
            State::Replacement { error_returned } => {
                if byte.is_none() || *error_returned {
                    return Step::Finished;
                }
                *error_returned = true;
                Step::Error
            }
            State::Utf16 {
                big_endian,
                lead_byte,
                lead_surrogate,
            } => {
                let Some(byte) = byte else {
                    if lead_byte.is_some() || lead_surrogate.is_some() {
                        *lead_byte = None;
                        *lead_surrogate = None;
                        return Step::Error;
                    }
                    return Step::Finished;
                };
                let Some(first) = lead_byte.take() else {
                    *lead_byte = Some(byte);
                    return Step::Continue;
                };
                let unit = if *big_endian {
                    (u16::from(first) << 8) | u16::from(byte)
                } else {
                    (u16::from(byte) << 8) | u16::from(first)
                };
                if let Some(lead) = lead_surrogate.take() {
                    if (0xDC00..=0xDFFF).contains(&unit) {
                        let c = 0x10000
                            + ((u32::from(lead) - 0xD800) << 10)
                            + (u32::from(unit) - 0xDC00);
                        return Step::One(c);
                    }
                    let [hi, lo] = unit.to_be_bytes();
                    if *big_endian {
                        queue.restore(&[hi, lo]);
                    } else {
                        queue.restore(&[lo, hi]);
                    }
                    return Step::Error;
                }
                match unit {
                    0xD800..=0xDBFF => {
                        *lead_surrogate = Some(unit);
                        Step::Continue
                    }
                    0xDC00..=0xDFFF => Step::Error,
                    _ => Step::One(unit.into()),
                }
            }
            State::XUserDefined => match byte {
                None => Step::Finished,
                Some(b) if b < 0x80 => Step::One(b.into()),
                Some(b) => Step::One(0xF780 + u32::from(b) - 0x80),
            },
        }
    }

    /// Whether a UTF-8 decoder sits between characters, so that whole valid
    /// runs can be copied at once.
    fn utf8_idle(&self) -> bool {
        matches!(self, State::Utf8 { needed: 0, .. })
    }
}

/// A streaming decoder: one instance of an encoding's decoder plus the
/// byte-order-mark handling around it.
#[derive(Clone, Debug)]
pub struct Decoder {
    label_encoding: Encoding,
    encoding: Encoding,
    mode: DecoderErrorMode,
    bom: BomHandling,
    /// Sniff: the BOM decision is made. Strip: the first character is out.
    bom_done: bool,
    /// Sniff: the first bytes, kept until they can be told apart from a BOM.
    head: Vec<u8>,
    state: State,
}

impl Decoder {
    pub fn new(encoding: Encoding, mode: DecoderErrorMode, bom: BomHandling) -> Decoder {
        Decoder {
            label_encoding: encoding,
            encoding,
            mode,
            bom,
            bom_done: false,
            head: Vec::new(),
            state: State::new(encoding),
        }
    }

    /// The encoding in use: the one given, or the one a sniffed byte order
    /// mark picked.
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Back to the state of a new decoder.
    pub fn reset(&mut self) {
        *self = Decoder::new(self.label_encoding, self.mode, self.bom);
    }

    /// Decode `input`, appending to `out`. With `last` false the decoder
    /// keeps an incomplete sequence for the next call; with `last` true it
    /// ends the stream and goes back to its initial state. In fatal mode the
    /// first error returns `Err` and resets the decoder.
    pub fn decode(
        &mut self,
        input: &[u8],
        last: bool,
        out: &mut String,
    ) -> Result<(), DecodeError> {
        if self.bom == BomHandling::Sniff && !self.bom_done {
            self.head.extend_from_slice(input);
            if !last && could_start_bom(&self.head) {
                return Ok(());
            }
            self.bom_done = true;
            let head = std::mem::take(&mut self.head);
            let mut skip = 0;
            if let Some((encoding, len)) = super::bom_sniff(&head) {
                self.encoding = encoding;
                self.state = State::new(encoding);
                skip = len;
            }
            return self.run(&head[skip..], last, out);
        }
        self.run(input, last, out)
    }

    fn run(&mut self, input: &[u8], last: bool, out: &mut String) -> Result<(), DecodeError> {
        let mut queue = Restored::new();
        let mut pos = 0;
        loop {
            let byte = if let Some(b) = queue.pop() {
                Some(b)
            } else if pos < input.len() {
                if self.state.utf8_idle() && (self.bom != BomHandling::Strip || self.bom_done) {
                    let rest = &input[pos..];
                    let valid = match std::str::from_utf8(rest) {
                        Ok(_) => rest.len(),
                        Err(e) => e.valid_up_to(),
                    };
                    if valid > 0 {
                        // The first `valid` bytes were just checked.
                        out.push_str(std::str::from_utf8(&rest[..valid]).unwrap());
                        pos += valid;
                        continue;
                    }
                }
                pos += 1;
                Some(input[pos - 1])
            } else if last {
                None
            } else {
                return Ok(());
            };
            match self.state.handle(byte, &mut queue) {
                Step::Continue => {}
                Step::Finished => {
                    self.reset();
                    return Ok(());
                }
                Step::Error => match self.mode {
                    DecoderErrorMode::Replacement => self.emit(0xFFFD, out),
                    DecoderErrorMode::Fatal => {
                        self.reset();
                        return Err(DecodeError);
                    }
                },
                Step::One(c) => self.emit(c, out),
                Step::Two(a, b) => {
                    self.emit(a, out);
                    self.emit(b, out);
                }
            }
        }
    }

    fn emit(&mut self, c: u32, out: &mut String) {
        if self.bom == BomHandling::Strip && !self.bom_done {
            self.bom_done = true;
            if c == 0xFEFF
                && matches!(
                    self.encoding,
                    Encoding::Utf8 | Encoding::Utf16Be | Encoding::Utf16Le
                )
            {
                return;
            }
        }
        out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
    }
}

/// Whether `head` is a proper prefix of a byte order mark, so more bytes
/// are needed to decide.
fn could_start_bom(head: &[u8]) -> bool {
    match head {
        [] | [0xEF] | [0xEF, 0xBB] => true,
        [0xFE] | [0xFF] => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(encoding: Encoding, chunks: &[&[u8]]) -> String {
        let mut d = Decoder::new(encoding, DecoderErrorMode::Replacement, BomHandling::Keep);
        let mut out = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            d.decode(chunk, i + 1 == chunks.len(), &mut out).unwrap();
        }
        out
    }

    #[test]
    fn utf8_split_across_chunks() {
        assert_eq!(run(Encoding::Utf8, &[b"a\xE2\x82", b"\xACb"]), "a\u{20AC}b");
        assert_eq!(run(Encoding::Utf8, &[b"a\xE2\x82", b""]), "a\u{FFFD}");
    }

    #[test]
    fn sniff_waits_for_the_whole_bom() {
        let mut d = Decoder::new(
            Encoding::Windows1252,
            DecoderErrorMode::Replacement,
            BomHandling::Sniff,
        );
        let mut out = String::new();
        d.decode(b"\xEF", false, &mut out).unwrap();
        d.decode(b"\xBB", false, &mut out).unwrap();
        assert_eq!(out, "");
        d.decode(b"\xBF\xC3\xA9", true, &mut out).unwrap();
        assert_eq!(out, "\u{E9}");
    }

    #[test]
    fn strip_drops_one_leading_feff() {
        let mut d = Decoder::new(
            Encoding::Utf8,
            DecoderErrorMode::Replacement,
            BomHandling::Strip,
        );
        let mut out = String::new();
        d.decode(b"\xEF\xBB\xBF\xEF\xBB\xBFx", true, &mut out)
            .unwrap();
        assert_eq!(out, "\u{FEFF}x");
    }

    #[test]
    fn fatal_stops_and_resets() {
        let mut d = Decoder::new(Encoding::Utf8, DecoderErrorMode::Fatal, BomHandling::Keep);
        let mut out = String::new();
        assert_eq!(d.decode(b"ok\xFF", true, &mut out), Err(DecodeError));
        out.clear();
        d.decode(b"ok", true, &mut out).unwrap();
        assert_eq!(out, "ok");
    }
}
