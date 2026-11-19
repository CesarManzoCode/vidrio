//! The painter seen from outside the rendering pipeline: what produces the
//! framebuffer of a top-level browsing context (vault/CONTRACT.md §3.5).
//! WebDriver screenshots read it. K3 has no style, layout or painting, so
//! [`BlankPainter`] stands in: it gives the 800×600 viewport in white.
//! K4 replaces it with the real painter.

use vjs::Agent;

use crate::dom::DocRef;

/// The default viewport (the one wptrunner uses).
pub const VIEWPORT_WIDTH: u32 = 800;
pub const VIEWPORT_HEIGHT: u32 = 600;

/// An RGBA8 framebuffer with premultiplied alpha, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Framebuffer {
    /// A framebuffer filled with one opaque color.
    pub fn filled(width: u32, height: u32, rgb: [u8; 3]) -> Framebuffer {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width as usize * height as usize {
            pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        Framebuffer {
            width,
            height,
            pixels,
        }
    }

    /// The pixels as straight-alpha RGB over white (a screenshot shows the
    /// canvas over the white of the viewport).
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() / 4 * 3);
        for p in self.pixels.chunks_exact(4) {
            let inv = 255 - u16::from(p[3]);
            for &c in &p[..3] {
                out.push((u16::from(c) + inv).min(255) as u8);
            }
        }
        out
    }

    /// The framebuffer encoded as a PNG file (8-bit RGB).
    pub fn to_png(&self) -> Vec<u8> {
        vgfx::png::encode(
            self.width,
            self.height,
            &self.to_rgb(),
            vgfx::png::ColorType::Rgb,
        )
    }
}

/// Paints the active document of a top-level browsing context.
pub trait Painter {
    fn paint(&self, agent: &mut Agent, doc: &DocRef) -> Framebuffer;
}

/// The provisional painter of K3: a white viewport.
pub struct BlankPainter;

impl Painter for BlankPainter {
    fn paint(&self, _agent: &mut Agent, _doc: &DocRef) -> Framebuffer {
        Framebuffer::filled(VIEWPORT_WIDTH, VIEWPORT_HEIGHT, [255, 255, 255])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_composites_premultiplied_pixels_over_white() {
        let fb = Framebuffer {
            width: 2,
            height: 1,
            pixels: vec![0, 0, 0, 0, 100, 0, 0, 255],
        };
        assert_eq!(fb.to_rgb(), vec![255, 255, 255, 100, 0, 0]);
    }

    #[test]
    fn blank_viewport_is_white_png() {
        let fb = Framebuffer::filled(VIEWPORT_WIDTH, VIEWPORT_HEIGHT, [255, 255, 255]);
        assert_eq!(fb.pixels.len(), 800 * 600 * 4);
        let png = fb.to_png();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(&png[16..24], &[0, 0, 3, 32, 0, 0, 2, 88]);
    }
}
