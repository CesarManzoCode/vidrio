//! Index lookups (Encoding §5): code point for a pointer, for decoders, and
//! pointer for a code point, for encoders. The pointer direction is built
//! once per index on first use.

use std::sync::OnceLock;

use crate::generated::encoding_cjk::{
    BIG5, EUC_KR, GB18030, GB18030_RANGES, ISO_2022_JP_KATAKANA, JIS0208, JIS0212,
};

fn lookup<T: Copy + Into<u32>>(table: &[T], pointer: usize) -> Option<u32> {
    table.get(pointer).map(|&c| c.into()).filter(|&c| c != 0)
}

pub fn big5_code_point(pointer: usize) -> Option<u32> {
    lookup(&BIG5, pointer)
}

pub fn euc_kr_code_point(pointer: usize) -> Option<u32> {
    lookup(&EUC_KR, pointer)
}

pub fn gb18030_code_point(pointer: usize) -> Option<u32> {
    lookup(&GB18030, pointer)
}

pub fn jis0208_code_point(pointer: usize) -> Option<u32> {
    lookup(&JIS0208, pointer)
}

pub fn jis0212_code_point(pointer: usize) -> Option<u32> {
    lookup(&JIS0212, pointer)
}

pub fn iso_2022_jp_katakana_code_point(pointer: usize) -> Option<u32> {
    lookup(&ISO_2022_JP_KATAKANA, pointer)
}

/// The index gb18030 ranges code point for `pointer`.
pub fn gb18030_ranges_code_point(pointer: u32) -> Option<u32> {
    if (pointer > 39419 && pointer < 189000) || pointer > 1237575 {
        return None;
    }
    if pointer == 7457 {
        return Some(0xE7C7);
    }
    let i = GB18030_RANGES.partition_point(|&(p, _)| p <= pointer);
    let (offset, code_point_offset) = GB18030_RANGES[i - 1];
    Some(code_point_offset + pointer - offset)
}

/// The index gb18030 ranges pointer for `code_point` (not ASCII and not in
/// index gb18030, so above the first range start).
pub fn gb18030_ranges_pointer(code_point: u32) -> u32 {
    if code_point == 0xE7C7 {
        return 7457;
    }
    let i = GB18030_RANGES.partition_point(|&(_, c)| c <= code_point);
    let (pointer_offset, offset) = GB18030_RANGES[i - 1];
    pointer_offset + code_point - offset
}

/// (code point, first pointer) of `table`, sorted by code point, skipping
/// the pointers `skip` rejects.
fn reverse<T: Copy + Into<u32>>(table: &[T], skip: impl Fn(usize) -> bool) -> Vec<(u32, u16)> {
    let mut out: Vec<(u32, u16)> = table
        .iter()
        .enumerate()
        .map(|(pointer, &c)| (Into::<u32>::into(c), pointer))
        .filter(|&(c, pointer)| c != 0 && !skip(pointer))
        .map(|(c, pointer)| (c, pointer as u16))
        .collect();
    // Stable: equal code points keep pointer order, so dedup keeps the first.
    out.sort_by_key(|&(c, _)| c);
    out.dedup_by_key(|&mut (c, _)| c);
    out
}

fn find(table: &[(u32, u16)], code_point: u32) -> Option<u16> {
    table
        .binary_search_by_key(&code_point, |&(c, _)| c)
        .ok()
        .map(|i| table[i].1)
}

/// The index pointer for `code_point` in index jis0208.
pub fn jis0208_pointer(code_point: u32) -> Option<u16> {
    static TABLE: OnceLock<Vec<(u32, u16)>> = OnceLock::new();
    find(
        TABLE.get_or_init(|| reverse(&JIS0208, |_| false)),
        code_point,
    )
}

/// The index Shift_JIS pointer for `code_point`: index jis0208 without
/// pointers 8272 to 8835.
pub fn shift_jis_pointer(code_point: u32) -> Option<u16> {
    static TABLE: OnceLock<Vec<(u32, u16)>> = OnceLock::new();
    find(
        TABLE.get_or_init(|| reverse(&JIS0208, |p| (8272..=8835).contains(&p))),
        code_point,
    )
}

/// The index Big5 pointer for `code_point`: index Big5 without the pointers
/// below (0xA1 − 0x81) × 157, the last pointer for six code points and the
/// first for the others.
pub fn big5_pointer(code_point: u32) -> Option<u16> {
    const LAST: [u32; 6] = [0x2550, 0x255E, 0x2561, 0x256A, 0x5341, 0x5345];
    const FIRST_POINTER: usize = (0xA1 - 0x81) * 157;
    if LAST.contains(&code_point) {
        return BIG5
            .iter()
            .rposition(|&c| c == code_point)
            .filter(|&p| p >= FIRST_POINTER)
            .map(|p| p as u16);
    }
    static TABLE: OnceLock<Vec<(u32, u16)>> = OnceLock::new();
    find(
        TABLE.get_or_init(|| reverse(&BIG5, |p| p < FIRST_POINTER)),
        code_point,
    )
}

/// The index pointer for `code_point` in index EUC-KR.
pub fn euc_kr_pointer(code_point: u32) -> Option<u16> {
    static TABLE: OnceLock<Vec<(u32, u16)>> = OnceLock::new();
    find(
        TABLE.get_or_init(|| reverse(&EUC_KR, |_| false)),
        code_point,
    )
}

/// The index pointer for `code_point` in index gb18030.
pub fn gb18030_pointer(code_point: u32) -> Option<u16> {
    static TABLE: OnceLock<Vec<(u32, u16)>> = OnceLock::new();
    find(
        TABLE.get_or_init(|| reverse(&GB18030, |_| false)),
        code_point,
    )
}

/// The byte (0x80 to 0xFF) a single-byte index maps to `code_point`.
pub fn single_byte_pointer(table: &[u16; 128], code_point: u32) -> Option<u8> {
    table
        .iter()
        .position(|&c| c != 0 && u32::from(c) == code_point)
        .map(|p| p as u8 + 0x80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gb18030_ranges_round_trip() {
        assert_eq!(gb18030_ranges_code_point(0), Some(0x80));
        assert_eq!(gb18030_ranges_pointer(0x80), 0);
        assert_eq!(gb18030_ranges_code_point(189000), Some(0x10000));
        assert_eq!(gb18030_ranges_pointer(0x10FFFF), 1237575);
        assert_eq!(gb18030_ranges_code_point(39420), None);
        assert_eq!(gb18030_ranges_code_point(7457), Some(0xE7C7));
        assert_eq!(gb18030_ranges_pointer(0xE7C7), 7457);
    }

    #[test]
    fn shift_jis_skips_the_nec_duplicates() {
        // U+7E8A is at pointer 8272 (NEC selection) and again at 10744 (IBM).
        assert_eq!(jis0208_pointer(0x7E8A), Some(8272));
        assert_eq!(shift_jis_pointer(0x7E8A), Some(10744));
    }

    #[test]
    fn big5_uses_the_last_pointer_for_box_drawing() {
        // U+2550 is at pointers 5247 and 18991.
        assert_eq!(big5_pointer(0x2550), Some(18991));
        assert_eq!(big5_code_point(5247), Some(0x2550));
    }
}
