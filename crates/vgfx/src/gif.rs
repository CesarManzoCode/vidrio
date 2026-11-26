//! A GIF decoder (GIF87a and GIF89a): the first frame, with the global
//! or local color table, the transparent index of its Graphic Control
//! Extension and interlacing, placed on a transparent canvas of the
//! logical screen size (the background color index is ignored, as
//! browsers do).

use crate::image::{Image, ImageError, MAX_SIDE, err};

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, ImageError> {
        let v = *self
            .b
            .get(self.pos)
            .ok_or_else(|| ImageError("truncated GIF".to_string()))?;
        self.pos += 1;
        Ok(v)
    }

    fn u16(&mut self) -> Result<u16, ImageError> {
        Ok(u16::from(self.u8()?) | u16::from(self.u8()?) << 8)
    }

    fn bytes(&mut self, n: usize) -> Result<&[u8], ImageError> {
        let end = self.pos + n;
        if end > self.b.len() {
            return err("truncated GIF");
        }
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn table(&mut self, size: usize) -> Result<Vec<[u8; 3]>, ImageError> {
        Ok(self
            .bytes(size * 3)?
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect())
    }

    /// The concatenated data sub-blocks up to the block terminator (a
    /// truncated file ends them).
    fn sub_blocks(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(n) = self.u8() {
            if n == 0 {
                break;
            }
            match self.bytes(n as usize) {
                Ok(s) => out.extend_from_slice(s),
                Err(_) => {
                    out.extend_from_slice(&self.b[self.pos..]);
                    self.pos = self.b.len();
                    break;
                }
            }
        }
        out
    }
}

/// Decodes the first frame of a GIF file.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
    if !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return err("not a GIF file");
    }
    let mut r = Reader { b: bytes, pos: 6 };
    let width = u32::from(r.u16()?);
    let height = u32::from(r.u16()?);
    let packed = r.u8()?;
    let _background = r.u8()?;
    let _aspect = r.u8()?;
    let global = if packed & 0x80 != 0 {
        r.table(2 << (packed & 7))?
    } else {
        Vec::new()
    };
    let mut transparent: Option<u8> = None;
    loop {
        match r.u8()? {
            0x21 => {
                let label = r.u8()?;
                let data = r.sub_blocks();
                if label == 0xF9 && data.len() >= 4 {
                    transparent = (data[0] & 1 != 0).then_some(data[3]);
                }
            }
            0x2C => {
                let left = u32::from(r.u16()?);
                let top = u32::from(r.u16()?);
                let fw = u32::from(r.u16()?);
                let fh = u32::from(r.u16()?);
                let packed = r.u8()?;
                let local = if packed & 0x80 != 0 {
                    Some(r.table(2 << (packed & 7))?)
                } else {
                    None
                };
                let interlaced = packed & 0x40 != 0;
                let min_code = r.u8()?;
                let data = r.sub_blocks();
                // A screen smaller than the frame grows to hold it.
                let w = width.max(left + fw);
                let h = height.max(top + fh);
                if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
                    return err("bad GIF size");
                }
                let table = local.as_ref().unwrap_or(&global);
                let indices = lzw(&data, min_code, fw as usize * fh as usize)?;
                let rows = row_order(fh, interlaced);
                let mut out = vec![0u8; w as usize * h as usize * 4];
                for (k, &y) in rows.iter().enumerate() {
                    for x in 0..fw {
                        let Some(&i) = indices.get(k * fw as usize + x as usize) else {
                            continue;
                        };
                        if Some(i) == transparent {
                            continue;
                        }
                        let Some(c) = table.get(i as usize) else {
                            continue;
                        };
                        let o = ((top + y) as usize * w as usize + (left + x) as usize) * 4;
                        out[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                    }
                }
                return Ok(Image::from_straight(w, h, out));
            }
            0x3B => return err("GIF without frames"),
            _ => return err("bad GIF block"),
        }
    }
}

/// The image rows in the order they are stored.
fn row_order(height: u32, interlaced: bool) -> Vec<u32> {
    if !interlaced {
        return (0..height).collect();
    }
    let mut rows = Vec::with_capacity(height as usize);
    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
        let mut y = start;
        while y < height {
            rows.push(y);
            y += step;
        }
    }
    rows
}

/// Decompresses GIF's variable-length LZW into at most `limit` indices
/// (a stream that ends early gives what it has).
fn lzw(data: &[u8], min_code: u8, limit: usize) -> Result<Vec<u8>, ImageError> {
    if !(1..=11).contains(&min_code) {
        return err("bad LZW code size");
    }
    let clear = 1u16 << min_code;
    let end = clear + 1;
    // Each entry: (prefix code, last byte, length).
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut length = vec![0u16; 4096];
    for i in 0..clear {
        suffix[i as usize] = i as u8;
        length[i as usize] = 1;
    }
    let mut next = end + 1;
    let mut size = u32::from(min_code) + 1;
    let mut prev: Option<u16> = None;
    let mut out = Vec::with_capacity(limit);
    let mut bits = 0u32;
    let mut nbits = 0u32;
    let mut pos = 0;
    let mut buf = Vec::new();
    while out.len() < limit {
        while nbits < size {
            let Some(&b) = data.get(pos) else {
                return Ok(out);
            };
            pos += 1;
            bits |= u32::from(b) << nbits;
            nbits += 8;
        }
        let code = (bits & ((1 << size) - 1)) as u16;
        bits >>= size;
        nbits -= size;
        if code == clear {
            next = end + 1;
            size = u32::from(min_code) + 1;
            prev = None;
            continue;
        }
        if code == end {
            break;
        }
        let Some(p) = prev else {
            if code >= clear {
                return Ok(out);
            }
            out.push(code as u8);
            prev = Some(code);
            continue;
        };
        let known = code < next;
        if !known && code != next {
            return Ok(out);
        }
        // The string of `code` (for the unknown code: prev's + its first).
        let start = if known { code } else { p };
        buf.clear();
        let mut c = start;
        loop {
            buf.push(suffix[c as usize]);
            if length[c as usize] <= 1 {
                break;
            }
            c = prefix[c as usize];
        }
        buf.reverse();
        let first = buf[0];
        if !known {
            buf.push(first);
        }
        out.extend_from_slice(&buf);
        if next < 4096 {
            prefix[next as usize] = p;
            suffix[next as usize] = first;
            length[next as usize] = length[p as usize] + 1;
            next += 1;
            if u32::from(next) == 1 << size && size < 12 {
                size += 1;
            }
        }
        prev = Some(code);
    }
    out.truncate(limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_two_color_gif_with_transparency() {
        // 2×2, global table [red, blue], index 1 transparent; pixels
        // 0 1 / 1 0 coded with minimum code size 2.
        let mut gif = b"GIF89a\x02\x00\x02\x00\x80\x00\x00".to_vec();
        gif.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
        gif.extend_from_slice(&[0x21, 0xF9, 4, 1, 0, 0, 1, 0]);
        gif.extend_from_slice(&[0x2C, 0, 0, 0, 0, 2, 0, 2, 0, 0]);
        // Codes (3 bits): clear 4, 0, 1, 1, 0, end 5.
        let codes = [4u32, 0, 1, 1, 0, 5];
        let sizes = [3u32, 3, 3, 3, 4, 4];
        let (mut acc, mut n, mut data) = (0u32, 0u32, Vec::new());
        for (c, s) in codes.iter().zip(sizes) {
            acc |= c << n;
            n += s;
            while n >= 8 {
                data.push(acc as u8);
                acc >>= 8;
                n -= 8;
            }
        }
        if n > 0 {
            data.push(acc as u8);
        }
        gif.push(2);
        gif.push(data.len() as u8);
        gif.extend_from_slice(&data);
        gif.extend_from_slice(&[0, 0x3B]);
        let img = decode(&gif).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(img.pixel(1, 0), [0, 0, 0, 0]);
        assert_eq!(img.pixel(0, 1), [0, 0, 0, 0]);
        assert_eq!(img.pixel(1, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn interlaced_rows_come_in_four_passes() {
        assert_eq!(row_order(10, true), vec![0, 8, 4, 2, 6, 1, 3, 5, 7, 9]);
    }
}
