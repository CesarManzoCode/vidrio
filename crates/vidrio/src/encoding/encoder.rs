//! Streaming encoders (Encoding §8 to §14). Input is Rust text, so it holds
//! only scalar values, as the standard requires of an encoder's input.

use super::Encoding;
use super::index;
use crate::generated::encoding_single_byte;

/// Error mode of an encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderErrorMode {
    /// The first unmappable code point stops encoding.
    Fatal,
    /// Each unmappable code point becomes `&#N;` (HTML forms).
    Html,
}

/// An unmappable code point in fatal mode: the code point the encoder
/// reports and how many bytes of the input were consumed, the code point
/// included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeError {
    pub code_point: u32,
    pub consumed: usize,
}

/// The result of one run of a handler on a code point.
enum Step {
    /// The code point was consumed.
    Done,
    /// Bytes were written and the same code point goes in again.
    Again,
    Error(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JpState {
    Ascii,
    Roman,
    Jis0208,
}

#[derive(Clone, Debug)]
enum State {
    Utf8,
    SingleByte(&'static [u16; 128]),
    Gb18030 { gbk: bool },
    Big5,
    EucJp,
    Iso2022Jp(JpState),
    ShiftJis,
    EucKr,
    XUserDefined,
}

impl State {
    fn new(encoding: Encoding) -> State {
        if let Some(table) = encoding_single_byte::index(encoding) {
            return State::SingleByte(table);
        }
        match encoding {
            Encoding::Gbk => State::Gb18030 { gbk: true },
            Encoding::Gb18030 => State::Gb18030 { gbk: false },
            Encoding::Big5 => State::Big5,
            Encoding::EucJp => State::EucJp,
            Encoding::Iso2022Jp => State::Iso2022Jp(JpState::Ascii),
            Encoding::ShiftJis => State::ShiftJis,
            Encoding::EucKr => State::EucKr,
            Encoding::XUserDefined => State::XUserDefined,
            _ => State::Utf8,
        }
    }

    /// The handler on the end of the queue: only ISO-2022-JP writes bytes.
    fn finish(&mut self, out: &mut Vec<u8>) {
        if let State::Iso2022Jp(state) = self {
            if *state != JpState::Ascii {
                *state = JpState::Ascii;
                out.extend_from_slice(b"\x1B(B");
            }
        }
    }

    fn handle(&mut self, c: char, out: &mut Vec<u8>) -> Step {
        let cp = c as u32;
        if cp < 0x80 && !matches!(self, State::Iso2022Jp(_)) {
            out.push(cp as u8);
            return Step::Done;
        }
        match self {
            State::Utf8 => {
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                Step::Done
            }
            State::SingleByte(table) => match index::single_byte_pointer(table, cp) {
                Some(b) => {
                    out.push(b);
                    Step::Done
                }
                None => Step::Error(cp),
            },
            State::Gb18030 { gbk } => {
                if cp == 0xE5E5 {
                    return Step::Error(cp);
                }
                if *gbk && cp == 0x20AC {
                    out.push(0x80);
                    return Step::Done;
                }
                if let Some(bytes) = gb18030_pua(cp) {
                    out.extend_from_slice(&bytes);
                    return Step::Done;
                }
                if let Some(pointer) = index::gb18030_pointer(cp) {
                    let pointer = u32::from(pointer);
                    let trail = pointer % 190;
                    let offset = if trail < 0x3F { 0x40 } else { 0x41 };
                    out.push((pointer / 190 + 0x81) as u8);
                    out.push((trail + offset) as u8);
                    return Step::Done;
                }
                if *gbk {
                    return Step::Error(cp);
                }
                let mut pointer = index::gb18030_ranges_pointer(cp);
                let byte1 = pointer / (10 * 126 * 10);
                pointer %= 10 * 126 * 10;
                let byte2 = pointer / (10 * 126);
                pointer %= 10 * 126;
                let byte3 = pointer / 10;
                let byte4 = pointer % 10;
                out.extend_from_slice(&[
                    (byte1 + 0x81) as u8,
                    (byte2 + 0x30) as u8,
                    (byte3 + 0x81) as u8,
                    (byte4 + 0x30) as u8,
                ]);
                Step::Done
            }
            State::Big5 => match index::big5_pointer(cp) {
                Some(pointer) => {
                    let pointer = u32::from(pointer);
                    let trail = pointer % 157;
                    let offset = if trail < 0x3F { 0x40 } else { 0x62 };
                    out.push((pointer / 157 + 0x81) as u8);
                    out.push((trail + offset) as u8);
                    Step::Done
                }
                None => Step::Error(cp),
            },
            State::EucJp => {
                match cp {
                    0xA5 => out.push(0x5C),
                    0x203E => out.push(0x7E),
                    0xFF61..=0xFF9F => out.extend_from_slice(&[0x8E, (cp - 0xFF61 + 0xA1) as u8]),
                    _ => {
                        let cp = if cp == 0x2212 { 0xFF0D } else { cp };
                        let Some(pointer) = index::jis0208_pointer(cp) else {
                            return Step::Error(c as u32);
                        };
                        out.push((pointer / 94 + 0xA1) as u8);
                        out.push((pointer % 94 + 0xA1) as u8);
                    }
                }
                Step::Done
            }
            State::Iso2022Jp(state) => {
                if matches!(*state, JpState::Ascii | JpState::Roman)
                    && matches!(cp, 0x0E | 0x0F | 0x1B)
                {
                    return Step::Error(0xFFFD);
                }
                if *state == JpState::Ascii && cp < 0x80 {
                    out.push(cp as u8);
                    return Step::Done;
                }
                if *state == JpState::Roman {
                    match cp {
                        0x5C | 0x7E => {}
                        0x00..=0x7F => {
                            out.push(cp as u8);
                            return Step::Done;
                        }
                        0xA5 => {
                            out.push(0x5C);
                            return Step::Done;
                        }
                        0x203E => {
                            out.push(0x7E);
                            return Step::Done;
                        }
                        _ => {}
                    }
                }
                if cp < 0x80 {
                    *state = JpState::Ascii;
                    out.extend_from_slice(b"\x1B(B");
                    return Step::Again;
                }
                if cp == 0xA5 || cp == 0x203E {
                    *state = JpState::Roman;
                    out.extend_from_slice(b"\x1B(J");
                    return Step::Again;
                }
                let mut target = if cp == 0x2212 { 0xFF0D } else { cp };
                if (0xFF61..=0xFF9F).contains(&target) {
                    target = index::iso_2022_jp_katakana_code_point((target - 0xFF61) as usize)
                        .unwrap_or(target);
                }
                let Some(pointer) = index::jis0208_pointer(target) else {
                    if *state == JpState::Jis0208 {
                        *state = JpState::Ascii;
                        out.extend_from_slice(b"\x1B(B");
                        return Step::Again;
                    }
                    return Step::Error(cp);
                };
                if *state != JpState::Jis0208 {
                    *state = JpState::Jis0208;
                    out.extend_from_slice(b"\x1B$B");
                    return Step::Again;
                }
                out.push((pointer / 94 + 0x21) as u8);
                out.push((pointer % 94 + 0x21) as u8);
                Step::Done
            }
            State::ShiftJis => {
                match cp {
                    0x80 => out.push(0x80),
                    0xA5 => out.push(0x5C),
                    0x203E => out.push(0x7E),
                    0xFF61..=0xFF9F => out.push((cp - 0xFF61 + 0xA1) as u8),
                    _ => {
                        let target = if cp == 0x2212 { 0xFF0D } else { cp };
                        let Some(pointer) = index::shift_jis_pointer(target) else {
                            return Step::Error(cp);
                        };
                        let lead = pointer / 188;
                        let lead_offset = if lead < 0x1F { 0x81 } else { 0xC1 };
                        let trail = pointer % 188;
                        let offset = if trail < 0x3F { 0x40 } else { 0x41 };
                        out.push((lead + lead_offset) as u8);
                        out.push((trail + offset) as u8);
                    }
                }
                Step::Done
            }
            State::EucKr => match index::euc_kr_pointer(cp) {
                Some(pointer) => {
                    out.push((pointer / 190 + 0x81) as u8);
                    out.push((pointer % 190 + 0x41) as u8);
                    Step::Done
                }
                None => Step::Error(cp),
            },
            State::XUserDefined => {
                if (0xF780..=0xF7FF).contains(&cp) {
                    out.push((cp - 0xF780 + 0x80) as u8);
                    Step::Done
                } else {
                    Step::Error(cp)
                }
            }
        }
    }
}

/// The gb18030 encoder's side table for Private Use code points that
/// GB18030-2022 moved (Encoding §10.2.2).
fn gb18030_pua(cp: u32) -> Option<[u8; 2]> {
    Some(match cp {
        0xE78D => [0xA6, 0xD9],
        0xE78E => [0xA6, 0xDA],
        0xE78F => [0xA6, 0xDB],
        0xE790 => [0xA6, 0xDC],
        0xE791 => [0xA6, 0xDD],
        0xE792 => [0xA6, 0xDE],
        0xE793 => [0xA6, 0xDF],
        0xE794 => [0xA6, 0xEC],
        0xE795 => [0xA6, 0xED],
        0xE796 => [0xA6, 0xF3],
        0xE81E => [0xFE, 0x59],
        0xE826 => [0xFE, 0x61],
        0xE82B => [0xFE, 0x66],
        0xE82C => [0xFE, 0x67],
        0xE832 => [0xFE, 0x6D],
        0xE843 => [0xFE, 0x7E],
        0xE854 => [0xFE, 0x90],
        0xE864 => [0xFE, 0xA0],
        _ => return None,
    })
}

/// A streaming encoder. Encodings without an encoder of their own
/// (replacement, UTF-16BE/LE) encode as UTF-8, as get an output encoding
/// says.
#[derive(Clone, Debug)]
pub struct Encoder {
    encoding: Encoding,
    state: State,
}

impl Encoder {
    pub fn new(encoding: Encoding) -> Encoder {
        let encoding = encoding.output_encoding();
        Encoder {
            encoding,
            state: State::new(encoding),
        }
    }

    /// The encoding written.
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Encode or fail (§6): encode `input` into `out` until the first
    /// unmappable code point. On `Err`, call again with
    /// `&input[err.consumed..]` and the same encoder to go on. With `last`,
    /// the end of the queue is processed once all of `input` is in.
    pub fn encode_or_fail(
        &mut self,
        input: &str,
        last: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), EncodeError> {
        for (i, c) in input.char_indices() {
            loop {
                match self.state.handle(c, out) {
                    Step::Done => break,
                    Step::Again => continue,
                    Step::Error(code_point) => {
                        return Err(EncodeError {
                            code_point,
                            consumed: i + c.len_utf8(),
                        });
                    }
                }
            }
        }
        if last {
            self.state.finish(out);
        }
        Ok(())
    }

    /// Encode `input` in `mode`. In html mode this never fails.
    pub fn encode(
        &mut self,
        input: &str,
        last: bool,
        out: &mut Vec<u8>,
        mode: EncoderErrorMode,
    ) -> Result<(), EncodeError> {
        let mut rest = input;
        loop {
            match self.encode_or_fail(rest, last, out) {
                Ok(()) => return Ok(()),
                Err(e) if mode == EncoderErrorMode::Html => {
                    out.extend_from_slice(format!("&#{};", e.code_point).as_bytes());
                    rest = &rest[e.consumed..];
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(encoding: Encoding, text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        Encoder::new(encoding)
            .encode(text, true, &mut out, EncoderErrorMode::Html)
            .unwrap();
        out
    }

    #[test]
    fn single_byte_unmappable_becomes_a_reference() {
        assert_eq!(
            html(Encoding::Windows1252, "\u{20AC}\u{2603}"),
            b"\x80&#9731;"
        );
    }

    #[test]
    fn iso_2022_jp_switches_and_returns_to_ascii() {
        assert_eq!(html(Encoding::Iso2022Jp, "a\u{A5}"), b"a\x1B(J\x5C\x1B(B");
        assert_eq!(html(Encoding::Iso2022Jp, "\u{1B}"), b"&#65533;");
    }

    #[test]
    fn gbk_and_gb18030_differ_on_the_euro_and_four_byte_forms() {
        assert_eq!(html(Encoding::Gbk, "\u{20AC}"), b"\x80");
        assert_eq!(html(Encoding::Gb18030, "\u{80}"), b"\x81\x30\x81\x30");
        assert_eq!(html(Encoding::Gbk, "\u{80}"), b"&#128;");
        assert_eq!(html(Encoding::Gb18030, "\u{E5E5}"), b"&#58853;");
    }

    #[test]
    fn utf16_encodes_as_utf8() {
        assert_eq!(Encoder::new(Encoding::Utf16Le).encoding(), Encoding::Utf8);
        assert_eq!(html(Encoding::Replacement, "\u{E9}"), "\u{E9}".as_bytes());
    }
}
