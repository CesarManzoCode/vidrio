//! The legacy `kern` table: horizontal format 0 subtables, in the
//! Microsoft (version 0) and Apple (version 1.0) headers.

use super::read::{i16_at, u16_at, u32_at};

/// Sum of the kerning values for the pair (left, right) in font units.
pub fn pair(d: &[u8], left: u16, right: u16) -> i32 {
    let mut total = 0i32;
    let Some(v) = u16_at(d, 0) else {
        return 0;
    };
    let apple = v == 1 && u16_at(d, 2) == Some(0);
    let (count, mut off) = if apple {
        (u32_at(d, 4).unwrap_or(0) as usize, 8usize)
    } else {
        (u16_at(d, 2).unwrap_or(0) as usize, 4usize)
    };
    for _ in 0..count {
        let (len, format, horizontal, cross, header) = if apple {
            let Some(len) = u32_at(d, off) else { break };
            let cov = u16_at(d, off + 4).unwrap_or(0);
            // Apple: bit 15 vertical, bit 14 cross-stream, low byte format.
            (
                len as usize,
                cov & 0xff,
                cov & 0x8000 == 0,
                cov & 0x4000 != 0,
                8,
            )
        } else {
            let Some(len) = u16_at(d, off + 2) else { break };
            let cov = u16_at(d, off + 4).unwrap_or(0);
            (len as usize, cov >> 8, cov & 1 != 0, cov & 4 != 0, 6)
        };
        if format == 0 && horizontal && !cross {
            if let Some(v) = format0(d, off + header, left, right) {
                total += v;
            }
        }
        if len == 0 {
            break;
        }
        off += len;
    }
    total
}

fn format0(d: &[u8], at: usize, left: u16, right: u16) -> Option<i32> {
    let n = u16_at(d, at)? as usize;
    let key = ((left as u32) << 16) | right as u32;
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let rec = at + 8 + mid * 6;
        let k = u32_at(d, rec)?;
        if k == key {
            return Some(i16_at(d, rec + 4)? as i32);
        }
        if k < key {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microsoft_format0() {
        let mut t = Vec::new();
        let w = |t: &mut Vec<u8>, v: u16| t.extend_from_slice(&v.to_be_bytes());
        w(&mut t, 0);
        w(&mut t, 1);
        // Subtable: version, length, coverage (horizontal, format 0).
        w(&mut t, 0);
        w(&mut t, 6 + 8 + 12);
        w(&mut t, 1);
        w(&mut t, 2);
        w(&mut t, 12);
        w(&mut t, 1);
        w(&mut t, 0);
        for (l, r, v) in [(3u16, 4u16, -50i16), (5, 6, 20)] {
            w(&mut t, l);
            w(&mut t, r);
            w(&mut t, v as u16);
        }
        assert_eq!(pair(&t, 3, 4), -50);
        assert_eq!(pair(&t, 5, 6), 20);
        assert_eq!(pair(&t, 4, 3), 0);
    }
}
