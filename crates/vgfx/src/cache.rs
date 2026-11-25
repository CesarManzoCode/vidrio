//! Cache of rasterized glyphs keyed by (face, glyph, size).

use std::collections::HashMap;
use std::sync::Arc;

use crate::font::Face;
use crate::raster::{Mask, rasterize};

/// Entries kept before the cache is emptied (simple bound on memory).
const MAX_ENTRIES: usize = 16384;

/// Sizes are keyed in 1/64 px; the horizontal subpixel offset in quarters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub face: u64,
    pub glyph: u16,
    pub size_64: u32,
    pub subpixel: u8,
}

impl GlyphKey {
    pub fn new(face: &Face, glyph: u16, size: f32, subpixel_x: f32) -> GlyphKey {
        let frac = subpixel_x - subpixel_x.floor();
        GlyphKey {
            face: face.id,
            glyph,
            size_64: (size * 64.0).round().max(0.0) as u32,
            subpixel: ((frac * 4.0).round() as u8) % 4,
        }
    }

    pub fn size(&self) -> f32 {
        self.size_64 as f32 / 64.0
    }
}

#[derive(Default)]
pub struct GlyphCache {
    map: HashMap<GlyphKey, Arc<Mask>>,
    hits: u64,
    misses: u64,
}

impl GlyphCache {
    pub fn new() -> GlyphCache {
        GlyphCache::default()
    }

    /// The coverage mask of `glyph` at `size` px, its origin at the pen
    /// position plus the horizontal subpixel offset `subpixel_x` (only its
    /// fractional part counts, in quarter pixels).
    pub fn get(&mut self, face: &Face, glyph: u16, size: f32, subpixel_x: f32) -> Arc<Mask> {
        let key = GlyphKey::new(face, glyph, size, subpixel_x);
        if let Some(m) = self.map.get(&key) {
            self.hits += 1;
            return m.clone();
        }
        self.misses += 1;
        if self.map.len() >= MAX_ENTRIES {
            self.map.clear();
        }
        let scale = key.size() / face.units_per_em() as f32;
        let mask = match face.outline(glyph) {
            Some(o) => rasterize(&o, scale, key.subpixel as f32 / 4.0, 0.0),
            None => Mask::default(),
        };
        let m = Arc::new(mask);
        self.map.insert(key, m.clone());
        m
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// (hits, misses) since creation.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_quantize_size_and_subpixel() {
        let k = |size: f32, sub: f32| GlyphKey {
            face: 1,
            glyph: 2,
            size_64: (size * 64.0).round() as u32,
            subpixel: (((sub - sub.floor()) * 4.0).round() as u8) % 4,
        };
        assert_eq!(k(16.0, 0.0), k(16.001, 3.0));
        assert_ne!(k(16.0, 0.0), k(16.0, 0.5));
        assert_eq!(k(16.0, 0.99).subpixel, 0);
        assert_eq!(k(16.5, 0.0).size_64, 1056);
    }
}
