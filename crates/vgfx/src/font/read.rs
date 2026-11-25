//! Big-endian readers over font data. Every read is bounds-checked and
//! returns `None` past the end, so parsers propagate with `?`.

#[derive(Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Reader<'a> {
        Reader { data, pos }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        if self.pos + n > self.data.len() {
            return None;
        }
        self.pos += n;
        Some(())
    }

    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    pub fn u8(&mut self) -> Option<u8> {
        let v = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    pub fn i8(&mut self) -> Option<i8> {
        self.u8().map(|v| v as i8)
    }

    pub fn u16(&mut self) -> Option<u16> {
        let b = self.bytes(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }

    pub fn u24(&mut self) -> Option<u32> {
        let b = self.bytes(3)?;
        Some(((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32)
    }

    pub fn u32(&mut self) -> Option<u32> {
        let b = self.bytes(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }

    /// 16.16 fixed-point.
    pub fn fixed(&mut self) -> Option<f32> {
        self.i32().map(|v| v as f32 / 65536.0)
    }

    /// 2.14 fixed-point.
    pub fn f2dot14(&mut self) -> Option<f32> {
        self.i16().map(|v| v as f32 / 16384.0)
    }

    pub fn tag(&mut self) -> Option<Tag> {
        self.u32().map(Tag)
    }
}

pub fn u8_at(d: &[u8], off: usize) -> Option<u8> {
    d.get(off).copied()
}

pub fn u16_at(d: &[u8], off: usize) -> Option<u16> {
    let b = d.get(off..off.checked_add(2)?)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

pub fn i16_at(d: &[u8], off: usize) -> Option<i16> {
    u16_at(d, off).map(|v| v as i16)
}

pub fn u32_at(d: &[u8], off: usize) -> Option<u32> {
    let b = d.get(off..off.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// An OpenType tag: four ASCII bytes as a big-endian `u32`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tag(pub u32);

impl Tag {
    pub const fn new(s: &[u8; 4]) -> Tag {
        Tag(u32::from_be_bytes(*s))
    }

    /// Parses a tag from text; shorter tags are padded with spaces.
    pub fn from_str(s: &str) -> Option<Tag> {
        let b = s.as_bytes();
        if b.is_empty() || b.len() > 4 || !b.iter().all(|c| (0x20..=0x7e).contains(c)) {
            return None;
        }
        let mut t = [b' '; 4];
        t[..b.len()].copy_from_slice(b);
        Some(Tag::new(&t))
    }

    pub fn bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

impl std::fmt::Debug for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'{}'", String::from_utf8_lossy(&self.bytes()))
    }
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_big_endian_and_stops_at_end() {
        let d = [0x01, 0x02, 0xff, 0xfe, 0x00, 0x01, 0x80];
        let mut r = Reader::new(&d);
        assert_eq!(r.u16(), Some(0x0102));
        assert_eq!(r.i16(), Some(-2));
        assert_eq!(r.u24(), Some(0x000180));
        assert_eq!(r.u8(), None);
    }

    #[test]
    fn tags_pad_with_spaces() {
        assert_eq!(Tag::from_str("cvt"), Some(Tag::new(b"cvt ")));
        assert_eq!(Tag::new(b"kern").to_string(), "kern");
        assert_eq!(Tag::from_str("toolong"), None);
    }
}
