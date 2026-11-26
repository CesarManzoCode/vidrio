//! The X keyboard map: keycodes to keysyms (the table of
//! `GetKeyboardMapping`, read once), and keysyms to the raw key values of
//! WebDriver (WebDriver §15.2.1), which the input code turns into `key`
//! and `code` of keyboard events.

/// The keysyms of each keycode from `min_keycode` on.
pub struct Keymap {
    pub min_keycode: u8,
    pub per_keycode: usize,
    pub keysyms: Vec<u32>,
}

/// The shift and lock bits of an X event's state.
const SHIFT_MASK: u16 = 1;
const LOCK_MASK: u16 = 2;

impl Keymap {
    /// The keysym of `keycode` with modifier `state`: the second column
    /// when Shift is down (or Caps Lock, for letters), else the first.
    pub fn keysym(&self, keycode: u8, state: u16) -> u32 {
        if keycode < self.min_keycode || self.per_keycode == 0 {
            return 0;
        }
        let base = usize::from(keycode - self.min_keycode) * self.per_keycode;
        let col = |i: usize| -> u32 {
            if i < self.per_keycode {
                self.keysyms.get(base + i).copied().unwrap_or(0)
            } else {
                0
            }
        };
        let lower = col(0);
        let upper = match col(1) {
            0 => lower,
            u => u,
        };
        let letter = char::from_u32(lower).is_some_and(|c| c.is_ascii_lowercase());
        let shifted = state & SHIFT_MASK != 0;
        let locked = state & LOCK_MASK != 0 && letter;
        if shifted != locked { upper } else { lower }
    }
}

/// The raw key value (a character, or a WebDriver code point in the
/// private use area for named keys) of keysym `sym`.
pub fn raw_key(sym: u32) -> Option<char> {
    let named = match sym {
        0xff08 => '\u{E003}',                                        // BackSpace
        0xff09 => '\u{E004}',                                        // Tab
        0xff0d => '\u{E007}',                                        // Return
        0xff13 => '\u{E00B}',                                        // Pause
        0xff1b => '\u{E00C}',                                        // Escape
        0xff50 => '\u{E011}',                                        // Home
        0xff51 => '\u{E012}',                                        // Left
        0xff52 => '\u{E013}',                                        // Up
        0xff53 => '\u{E014}',                                        // Right
        0xff54 => '\u{E015}',                                        // Down
        0xff55 => '\u{E00E}',                                        // Prior
        0xff56 => '\u{E00F}',                                        // Next
        0xff57 => '\u{E010}',                                        // End
        0xff63 => '\u{E016}',                                        // Insert
        0xff8d => '\u{E007}',                                        // KP_Enter
        0xffe1 => '\u{E008}',                                        // Shift_L
        0xffe2 => '\u{E050}',                                        // Shift_R
        0xffe3 => '\u{E009}',                                        // Control_L
        0xffe4 => '\u{E051}',                                        // Control_R
        0xffe9 => '\u{E00A}',                                        // Alt_L
        0xffea => '\u{E052}',                                        // Alt_R
        0xffeb | 0xffe7 => '\u{E03D}',                               // Super_L, Meta_L
        0xffec | 0xffe8 => '\u{E053}',                               // Super_R, Meta_R
        0xffff => '\u{E017}',                                        // Delete
        0xffbe..=0xffc9 => char::from_u32(0xE031 + (sym - 0xffbe))?, // F1–F12
        // Latin-1 keysyms are their code points.
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(sym)?,
        // Unicode keysyms.
        0x0100_0100..=0x0110_ffff => char::from_u32(sym - 0x0100_0000)?,
        _ => return None,
    };
    Some(named)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms_become_raw_keys() {
        assert_eq!(raw_key(0x61), Some('a'));
        assert_eq!(raw_key(0xff0d), Some('\u{E007}'));
        assert_eq!(raw_key(0xffbe), Some('\u{E031}'));
        assert_eq!(raw_key(0x0100_20ac), Some('€'));
        assert_eq!(raw_key(0xfe03), None);
    }

    #[test]
    fn shift_and_caps_lock_pick_the_column() {
        let map = Keymap {
            min_keycode: 8,
            per_keycode: 2,
            // Keycode 8: a/A; keycode 9: 1/!.
            keysyms: vec![0x61, 0x41, 0x31, 0x21],
        };
        assert_eq!(map.keysym(8, 0), 0x61);
        assert_eq!(map.keysym(8, SHIFT_MASK), 0x41);
        assert_eq!(map.keysym(8, LOCK_MASK), 0x41);
        assert_eq!(map.keysym(9, LOCK_MASK), 0x31);
        assert_eq!(map.keysym(9, SHIFT_MASK), 0x21);
        assert_eq!(map.keysym(3, 0), 0);
    }
}
