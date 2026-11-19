//! A PNG encoder (ISO/IEC 15948): 8-bit RGB or RGBA, no interlacing, the
//! `None` filter on every row and a zlib stream from vnet.

use vnet::checksum::crc32;
use vnet::deflate::{Level, zlib_compress};

/// The pixel layout of the input and of the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorType {
    /// Three bytes per pixel (PNG color type 2).
    Rgb,
    /// Four bytes per pixel, straight (not premultiplied) alpha (type 6).
    Rgba,
}

impl ColorType {
    fn channels(self) -> usize {
        match self {
            ColorType::Rgb => 3,
            ColorType::Rgba => 4,
        }
    }

    fn code(self) -> u8 {
        match self {
            ColorType::Rgb => 2,
            ColorType::Rgba => 6,
        }
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Encodes `pixels` (rows top to bottom, `width * height` pixels laid out
/// as `color`) as a PNG file. Panics if the buffer has the wrong size.
pub fn encode(width: u32, height: u32, pixels: &[u8], color: ColorType) -> Vec<u8> {
    let stride = width as usize * color.channels();
    assert_eq!(pixels.len(), stride * height as usize, "pixel buffer size");
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in pixels.chunks(stride.max(1)).take(height as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    if stride == 0 {
        raw = vec![0; height as usize];
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // Bit depth 8, color type, deflate, adaptive filtering, no interlace.
    ihdr.extend_from_slice(&[8, color.code(), 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_compress(&raw, Level::default()));
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_signature_header_and_chunks() {
        let png = encode(2, 1, &[255, 0, 0, 0, 0, 255], ColorType::Rgb);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[8..16], b"\x00\x00\x00\x0dIHDR");
        assert_eq!(&png[16..29], &[0, 0, 0, 2, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
        let crc = crc32(&png[12..29]);
        assert_eq!(&png[29..33], &crc.to_be_bytes());
        assert_eq!(
            &png[png.len() - 12..],
            b"\x00\x00\x00\x00IEND\xae\x42\x60\x82"
        );
    }

    #[test]
    fn image_data_inflates_to_filtered_rows() {
        let pixels = [1, 2, 3, 4, 5, 6, 7, 8];
        let png = encode(1, 2, &pixels, ColorType::Rgba);
        let len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let raw = vnet::inflate::zlib_decompress(&png[41..41 + len]).unwrap();
        assert_eq!(raw, vec![0, 1, 2, 3, 4, 0, 5, 6, 7, 8]);
    }
}
