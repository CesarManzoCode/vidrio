//! CRC-32 (ISO 3309, used by gzip and PNG) and Adler-32 (RFC 1950).

const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = make_crc_table();

/// Running CRC-32.
#[derive(Clone, Copy, Debug)]
pub struct Crc32 {
    state: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub fn new() -> Self {
        Crc32 { state: 0xFFFF_FFFF }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut c = self.state;
        for &b in data {
            c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        self.state = c;
    }

    pub fn value(&self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.value()
}

/// Running Adler-32.
#[derive(Clone, Copy, Debug)]
pub struct Adler32 {
    a: u32,
    b: u32,
}

impl Default for Adler32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Adler32 {
    const MOD: u32 = 65521;
    // Largest n such that 255 n (n + 1) / 2 + (n + 1) (MOD - 1) fits in u32.
    const NMAX: usize = 5552;

    pub fn new() -> Self {
        Adler32 { a: 1, b: 0 }
    }

    pub fn update(&mut self, data: &[u8]) {
        for chunk in data.chunks(Self::NMAX) {
            for &x in chunk {
                self.a += x as u32;
                self.b += self.a;
            }
            self.a %= Self::MOD;
            self.b %= Self::MOD;
        }
    }

    pub fn value(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

pub fn adler32(data: &[u8]) -> u32 {
    let mut a = Adler32::new();
    a.update(data);
    a.value()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data: Vec<u8> = (0..20000u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut c = Crc32::new();
        let mut a = Adler32::new();
        for part in data.chunks(777) {
            c.update(part);
            a.update(part);
        }
        assert_eq!(c.value(), crc32(&data));
        assert_eq!(a.value(), adler32(&data));
    }
}
