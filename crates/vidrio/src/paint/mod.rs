//! Painting (`vault/CONTRACT.md` §3.5): the fragment tree of a laid out
//! document becomes a flat paint list in the order of CSS 2 Appendix E
//! ([`build`]), which is rasterized into an RGBA8 framebuffer with
//! premultiplied alpha ([`render`]). [`DocumentPainter`] runs style,
//! layout and painting for the active document of a top-level browsing
//! context; WebDriver screenshots read its framebuffer.

pub mod background;
pub mod border;
pub mod build;
pub mod color;
pub mod gradient;
pub mod images;
pub mod list;
pub mod render;

use vgfx::font::system::WebFace;
use vjs::Agent;

pub use build::{NoResources, Resources};
pub use list::DisplayList;

use crate::dom::{DocRef, NodeId};

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

    /// The premultiplied pixel at (`x`, `y`).
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
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

impl From<vgfx::canvas::Pixmap> for Framebuffer {
    fn from(p: vgfx::canvas::Pixmap) -> Framebuffer {
        Framebuffer {
            width: p.width,
            height: p.height,
            pixels: p.data,
        }
    }
}

/// Paints the active document of a top-level browsing context.
pub trait Painter {
    fn paint(&self, agent: &mut Agent, doc: &DocRef) -> Framebuffer;
}

/// What a paint of a document uses besides the document.
pub struct PaintOptions {
    /// The viewport size in CSS pixels.
    pub viewport: (u32, u32),
    /// Whether the system fonts (D-15) are used.
    pub system_fonts: bool,
    /// Extra web fonts searched before the document's own.
    pub web_fonts: Vec<WebFace>,
}

impl Default for PaintOptions {
    fn default() -> PaintOptions {
        PaintOptions {
            viewport: (VIEWPORT_WIDTH, VIEWPORT_HEIGHT),
            system_fonts: true,
            web_fonts: Vec::new(),
        }
    }
}

/// Style, layout and the paint list of `doc`.
pub fn display_list(doc: &DocRef, res: &dyn ImageSource, options: &PaintOptions) -> DisplayList {
    let styles = crate::css::style::styles(doc);
    let mut fonts = options.web_fonts.clone();
    fonts.extend(crate::bindings::fonts::web_faces(doc));
    let natural = |n: NodeId| res.natural_size(n);
    let (w, h) = options.viewport;
    let d = doc.borrow();
    let tree = crate::layout::layout(
        &d,
        &styles,
        crate::layout::Options {
            viewport: (f64::from(w), f64::from(h)),
            web_fonts: fonts,
            system_fonts: options.system_fonts,
            images: &natural,
        },
    );
    let body = if d.is_html() { d.body() } else { None };
    build::build(&tree, res.resources(), body)
}

/// Renders `doc` into a framebuffer.
pub fn paint_document(doc: &DocRef, res: &dyn ImageSource, options: &PaintOptions) -> Framebuffer {
    render::render(&display_list(doc, res, options)).into()
}

/// Images for layout (natural sizes) and painting.
pub trait ImageSource {
    fn natural_size(&self, node: NodeId) -> Option<(f64, f64)>;
    fn resources(&self) -> &dyn Resources;
}

impl ImageSource for images::DocumentImages {
    fn natural_size(&self, node: NodeId) -> Option<(f64, f64)> {
        images::DocumentImages::natural_size(self, node)
    }

    fn resources(&self) -> &dyn Resources {
        self
    }
}

impl ImageSource for NoResources {
    fn natural_size(&self, _: NodeId) -> Option<(f64, f64)> {
        None
    }

    fn resources(&self) -> &dyn Resources {
        self
    }
}

/// The painter of the browser: style, layout, the paint list and its
/// rasterization, with the document's images and web fonts.
#[derive(Default)]
pub struct DocumentPainter {
    pub options: PaintOptions,
}

impl DocumentPainter {
    pub fn new() -> DocumentPainter {
        DocumentPainter::default()
    }
}

impl Painter for DocumentPainter {
    fn paint(&self, agent: &mut Agent, doc: &DocRef) -> Framebuffer {
        // Images that appeared since the last rendering update start
        // loading now (they show in a later screenshot).
        images::update_document(agent, doc);
        let res = images::DocumentImages::new(doc);
        paint_document(doc, &res, &self.options)
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
    fn white_viewport_is_an_800_by_600_png() {
        let fb = Framebuffer::filled(VIEWPORT_WIDTH, VIEWPORT_HEIGHT, [255, 255, 255]);
        assert_eq!(fb.pixels.len(), 800 * 600 * 4);
        let png = fb.to_png();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(&png[16..24], &[0, 0, 3, 32, 0, 0, 2, 88]);
    }
}
