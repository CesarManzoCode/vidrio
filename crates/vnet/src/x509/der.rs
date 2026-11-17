//! Minimal DER reader (X.690) for certificates and signatures.

/// Tags used by X.509.
pub mod tag {
    pub const BOOLEAN: u8 = 0x01;
    pub const INTEGER: u8 = 0x02;
    pub const BIT_STRING: u8 = 0x03;
    pub const OCTET_STRING: u8 = 0x04;
    pub const NULL: u8 = 0x05;
    pub const OID: u8 = 0x06;
    pub const UTF8_STRING: u8 = 0x0c;
    pub const PRINTABLE_STRING: u8 = 0x13;
    pub const T61_STRING: u8 = 0x14;
    pub const IA5_STRING: u8 = 0x16;
    pub const UTC_TIME: u8 = 0x17;
    pub const GENERALIZED_TIME: u8 = 0x18;
    pub const UNIVERSAL_STRING: u8 = 0x1c;
    pub const BMP_STRING: u8 = 0x1e;
    pub const SEQUENCE: u8 = 0x30;
    pub const SET: u8 = 0x31;

    /// `[n]` constructed context tag (EXPLICIT).
    pub const fn context(n: u8) -> u8 {
        0xa0 | n
    }

    /// `[n]` primitive context tag (IMPLICIT on a primitive type).
    pub const fn context_primitive(n: u8) -> u8 {
        0x80 | n
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DerError;

pub type DerResult<T> = Result<T, DerError>;

/// One TLV: tag, contents, and the whole encoding.
#[derive(Clone, Copy, Debug)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub value: &'a [u8],
    pub raw: &'a [u8],
}

#[derive(Clone, Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Next TLV. Only single-byte tags and definite lengths (DER) are accepted.
    pub fn read(&mut self) -> DerResult<Tlv<'a>> {
        let start = self.pos;
        let tag = *self.data.get(self.pos).ok_or(DerError)?;
        if tag & 0x1f == 0x1f {
            return Err(DerError);
        }
        let first = *self.data.get(self.pos + 1).ok_or(DerError)?;
        let mut pos = self.pos + 2;
        let len = if first < 0x80 {
            first as usize
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 || n > 4 {
                return Err(DerError);
            }
            let bytes = self.data.get(pos..pos + n).ok_or(DerError)?;
            pos += n;
            let mut len = 0usize;
            for &b in bytes {
                len = (len << 8) | b as usize;
            }
            // DER: the shortest form.
            if len < 0x80 || bytes[0] == 0 {
                return Err(DerError);
            }
            len
        };
        let end = pos.checked_add(len).ok_or(DerError)?;
        let value = self.data.get(pos..end).ok_or(DerError)?;
        self.pos = end;
        Ok(Tlv {
            tag,
            value,
            raw: &self.data[start..end],
        })
    }

    /// Contents of the next TLV, which must have tag `t`.
    pub fn expect(&mut self, t: u8) -> DerResult<&'a [u8]> {
        let tlv = self.read()?;
        if tlv.tag != t {
            return Err(DerError);
        }
        Ok(tlv.value)
    }

    /// The next TLV if it has tag `t`.
    pub fn optional(&mut self, t: u8) -> DerResult<Option<&'a [u8]>> {
        if self.peek_tag() == Some(t) {
            Ok(Some(self.expect(t)?))
        } else {
            Ok(None)
        }
    }

    /// A non-negative INTEGER as big-endian bytes without the sign octet.
    pub fn unsigned_integer(&mut self) -> DerResult<&'a [u8]> {
        let v = self.expect(tag::INTEGER)?;
        match v {
            [] => Err(DerError),
            [b, ..] if b & 0x80 != 0 => Err(DerError),
            [0, rest @ ..] if !rest.is_empty() => {
                if rest[0] & 0x80 == 0 {
                    return Err(DerError);
                }
                Ok(rest)
            }
            _ => Ok(v),
        }
    }

    /// A small non-negative INTEGER.
    pub fn small_integer(&mut self) -> DerResult<u64> {
        let v = self.unsigned_integer()?;
        if v.len() > 8 {
            return Err(DerError);
        }
        Ok(v.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64))
    }

    pub fn boolean(&mut self) -> DerResult<bool> {
        match self.expect(tag::BOOLEAN)? {
            [0x00] => Ok(false),
            [0xff] => Ok(true),
            _ => Err(DerError),
        }
    }

    /// BIT STRING contents with no unused bits.
    pub fn bit_string_bytes(&mut self) -> DerResult<&'a [u8]> {
        match self.expect(tag::BIT_STRING)? {
            [0, rest @ ..] => Ok(rest),
            _ => Err(DerError),
        }
    }
}
