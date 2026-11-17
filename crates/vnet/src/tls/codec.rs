//! Reading and writing the TLS presentation language (RFC 8446 §3).

/// Cursor over received bytes; every read fails with `decode_error` when
/// the data runs out.
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Truncated;

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Cursor<'a> {
        Cursor { data, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Truncated> {
        let end = self.pos.checked_add(n).ok_or(Truncated)?;
        let s = self.data.get(self.pos..end).ok_or(Truncated)?;
        self.pos = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, Truncated> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Truncated> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u24(&mut self) -> Result<usize, Truncated> {
        let b = self.take(3)?;
        Ok(((b[0] as usize) << 16) | ((b[1] as usize) << 8) | b[2] as usize)
    }

    /// A vector with a one-byte length.
    pub fn vec8(&mut self) -> Result<&'a [u8], Truncated> {
        let n = self.u8()? as usize;
        self.take(n)
    }

    pub fn vec16(&mut self) -> Result<&'a [u8], Truncated> {
        let n = self.u16()? as usize;
        self.take(n)
    }

    pub fn vec24(&mut self) -> Result<&'a [u8], Truncated> {
        let n = self.u24()?;
        self.take(n)
    }
}

pub fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn put_u24(out: &mut Vec<u8>, v: usize) {
    out.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
}

pub fn put_vec8(out: &mut Vec<u8>, data: &[u8]) {
    out.push(data.len() as u8);
    out.extend_from_slice(data);
}

pub fn put_vec16(out: &mut Vec<u8>, data: &[u8]) {
    put_u16(out, data.len() as u16);
    out.extend_from_slice(data);
}

pub fn put_vec24(out: &mut Vec<u8>, data: &[u8]) {
    put_u24(out, data.len());
    out.extend_from_slice(data);
}

/// A handshake message: type, 24-bit length, body.
pub fn handshake_message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.push(kind);
    put_vec24(&mut out, body);
    out
}

/// An extension: type and 16-bit length-prefixed data.
pub fn put_extension(out: &mut Vec<u8>, kind: u16, data: &[u8]) {
    put_u16(out, kind);
    put_vec16(out, data);
}
