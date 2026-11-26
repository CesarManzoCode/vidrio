//! PNG (ISO/IEC 15948). The encoder writes 8-bit RGB or RGBA, no
//! interlacing, the `None` filter on every row and a zlib stream from
//! vnet. The decoder reads every color type and bit depth, Adam7
//! interlacing, `PLTE` and `tRNS`, and the first frame of an APNG
//! (`acTL`, `fcTL`, `fdAT`); 16-bit samples are rounded to 8 bits.
//! Ancillary color chunks (`gAMA`, `cHRM`, `iCCP`, `sRGB`) are ignored.

use vnet::checksum::crc32;
use vnet::deflate::{Level, zlib_compress};

use crate::image::{Image, ImageError, MAX_SIDE, err};

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

// ----- decoding --------------------------------------------------------------------

/// The header of an image or of an APNG frame.
#[derive(Clone, Copy, Debug)]
struct Header {
    width: u32,
    height: u32,
    depth: u8,
    color: u8,
    interlace: bool,
}

impl Header {
    fn channels(&self) -> usize {
        match self.color {
            0 => 1,
            2 => 3,
            3 => 1,
            4 => 2,
            6 => 4,
            _ => 0,
        }
    }

    fn valid(&self) -> bool {
        let depth_ok = match self.color {
            0 => matches!(self.depth, 1 | 2 | 4 | 8 | 16),
            3 => matches!(self.depth, 1 | 2 | 4 | 8),
            2 | 4 | 6 => matches!(self.depth, 8 | 16),
            _ => false,
        };
        depth_ok
            && self.width > 0
            && self.height > 0
            && self.width <= MAX_SIDE
            && self.height <= MAX_SIDE
    }

    /// Bytes per complete pixel, at least 1 (the filter's offset).
    fn bpp(&self) -> usize {
        (self.channels() * self.depth as usize).div_ceil(8).max(1)
    }

    fn row_bytes(&self, width: u32) -> usize {
        (width as usize * self.channels() * self.depth as usize).div_ceil(8)
    }
}

/// The color the transparency chunk makes transparent, or the palette
/// alphas.
#[derive(Clone, Debug, Default)]
struct Trns {
    key: Option<[u16; 3]>,
    alpha: Vec<u8>,
}

/// An APNG frame control: the frame's size and offset.
#[derive(Clone, Copy, Debug)]
struct FrameControl {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn frame_control(d: &[u8]) -> Option<FrameControl> {
    (d.len() >= 26).then(|| FrameControl {
        width: be32(&d[4..]),
        height: be32(&d[8..]),
        x: be32(&d[12..]),
        y: be32(&d[16..]),
    })
}

/// Decodes a PNG file; for an APNG, its first frame.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return err("not a PNG file");
    }
    let mut pos = 8;
    let mut header: Option<Header> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns = Trns::default();
    let mut idat = Vec::new();
    let mut animated = false;
    // The frame control before the image data, if any (the default image
    // is then the first frame).
    let mut idat_control: Option<FrameControl> = None;
    let mut seen_idat = false;
    let mut first_frame: Option<(FrameControl, Vec<u8>)> = None;
    let mut frame_done = false;
    while pos + 8 <= bytes.len() {
        let len = be32(&bytes[pos..]) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let start = pos + 8;
        let Some(end) = start.checked_add(len).filter(|&e| e <= bytes.len()) else {
            // A truncated chunk ends the file (what arrived is decoded).
            break;
        };
        let data = &bytes[start..end];
        pos = end + 4;
        match kind {
            b"IHDR" => {
                if data.len() < 13 {
                    return err("short IHDR");
                }
                let h = Header {
                    width: be32(data),
                    height: be32(&data[4..]),
                    depth: data[8],
                    color: data[9],
                    interlace: data[12] == 1,
                };
                if !h.valid() || data[10] != 0 || data[11] != 0 || data[12] > 1 {
                    return err("unsupported IHDR");
                }
                header = Some(h);
            }
            b"PLTE" => {
                palette = data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            }
            b"tRNS" => {
                let Some(h) = header else {
                    continue;
                };
                match h.color {
                    0 if data.len() >= 2 => {
                        let g = u16::from_be_bytes([data[0], data[1]]);
                        trns.key = Some([g, g, g]);
                    }
                    2 if data.len() >= 6 => {
                        let c = |i: usize| u16::from_be_bytes([data[i], data[i + 1]]);
                        trns.key = Some([c(0), c(2), c(4)]);
                    }
                    3 => trns.alpha = data.to_vec(),
                    _ => {}
                }
            }
            b"acTL" => animated = true,
            b"fcTL" => {
                let fc = frame_control(data);
                if !seen_idat {
                    idat_control = fc;
                } else if first_frame.is_none() {
                    if let Some(fc) = fc {
                        first_frame = Some((fc, Vec::new()));
                    }
                } else {
                    frame_done = true;
                }
            }
            b"fdAT" => {
                if let Some((_, buf)) = &mut first_frame {
                    if !frame_done && data.len() >= 4 {
                        buf.extend_from_slice(&data[4..]);
                    }
                }
            }
            b"IDAT" => {
                seen_idat = true;
                idat.extend_from_slice(data);
            }
            b"IEND" => break,
            _ => {}
        }
    }
    let Some(h) = header else {
        return err("no IHDR");
    };
    if h.color == 3 && palette.is_empty() {
        return err("no PLTE");
    }
    let use_frame = animated && idat_control.is_none() && first_frame.is_some();
    if !use_frame {
        let raw = vnet::inflate::zlib_decompress(&idat)
            .map_err(|_| ImageError("bad zlib stream".to_string()))?;
        return decode_pixels(&h, &raw, &palette, &trns);
    }
    // The default image is not part of the animation: the first frame
    // is the first `fcTL` with its `fdAT` chunks, over a transparent
    // canvas.
    let (fc, data) = first_frame.expect("a first frame");
    if fc.width == 0
        || fc.height == 0
        || u64::from(fc.x) + u64::from(fc.width) > u64::from(h.width)
        || u64::from(fc.y) + u64::from(fc.height) > u64::from(h.height)
    {
        return err("bad APNG frame");
    }
    let fh = Header {
        width: fc.width,
        height: fc.height,
        ..h
    };
    let raw = vnet::inflate::zlib_decompress(&data)
        .map_err(|_| ImageError("bad zlib stream".to_string()))?;
    let frame = decode_pixels(&fh, &raw, &palette, &trns)?;
    let mut out = Image::transparent(h.width, h.height);
    for y in 0..fc.height {
        let src = (y as usize * fc.width as usize) * 4;
        let dst = ((fc.y + y) as usize * h.width as usize + fc.x as usize) * 4;
        let n = fc.width as usize * 4;
        out.data[dst..dst + n].copy_from_slice(&frame.data[src..src + n]);
    }
    Ok(out)
}

/// The Adam7 passes: (x start, y start, x step, y step).
const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

fn decode_pixels(
    h: &Header,
    raw: &[u8],
    palette: &[[u8; 3]],
    trns: &Trns,
) -> Result<Image, ImageError> {
    let mut out = vec![0u8; h.width as usize * h.height as usize * 4];
    let mut pos = 0;
    let passes: Vec<(u32, u32, u32, u32)> = if h.interlace {
        ADAM7.to_vec()
    } else {
        vec![(0, 0, 1, 1)]
    };
    for (x0, y0, dx, dy) in passes {
        if x0 >= h.width || y0 >= h.height {
            continue;
        }
        let pw = (h.width - x0).div_ceil(dx);
        let ph = (h.height - y0).div_ceil(dy);
        let stride = h.row_bytes(pw);
        let bpp = h.bpp();
        let mut prev = vec![0u8; stride];
        let mut row = vec![0u8; stride];
        for py in 0..ph {
            if pos >= raw.len() {
                // Missing rows stay transparent (a truncated stream).
                break;
            }
            let filter = raw[pos];
            pos += 1;
            let avail = (raw.len() - pos).min(stride);
            row[..avail].copy_from_slice(&raw[pos..pos + avail]);
            row[avail..].fill(0);
            pos += avail;
            unfilter(filter, &mut row, &prev, bpp)?;
            let y = y0 + py * dy;
            for px in 0..pw {
                let x = x0 + px * dx;
                let rgba = pixel(h, &row, px as usize, palette, trns);
                let i = (y as usize * h.width as usize + x as usize) * 4;
                out[i..i + 4].copy_from_slice(&rgba);
            }
            std::mem::swap(&mut prev, &mut row);
        }
    }
    Ok(Image::from_straight(h.width, h.height, out))
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn unfilter(filter: u8, row: &mut [u8], prev: &[u8], bpp: usize) -> Result<(), ImageError> {
    match filter {
        0 => {}
        1 => {
            for i in bpp..row.len() {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        2 => {
            for i in 0..row.len() {
                row[i] = row[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..row.len() {
                let a = if i >= bpp { u16::from(row[i - bpp]) } else { 0 };
                row[i] = row[i].wrapping_add(((a + u16::from(prev[i])) / 2) as u8);
            }
        }
        4 => {
            for i in 0..row.len() {
                let (a, c) = if i >= bpp {
                    (row[i - bpp], prev[i - bpp])
                } else {
                    (0, 0)
                };
                row[i] = row[i].wrapping_add(paeth(a, prev[i], c));
            }
        }
        _ => return err("bad filter type"),
    }
    Ok(())
}

/// Sample `i` of a row at bit depth `depth` (below 8: packed from the
/// most significant bit).
fn sample(row: &[u8], depth: u8, i: usize) -> u16 {
    match depth {
        8 => u16::from(row[i]),
        16 => u16::from_be_bytes([row[2 * i], row[2 * i + 1]]),
        d => {
            let per = 8 / d as usize;
            let byte = row[i / per];
            let shift = 8 - d as usize * (i % per + 1);
            (u16::from(byte) >> shift) & ((1 << d) - 1)
        }
    }
}

/// A sample scaled to 8 bits.
fn to8(v: u16, depth: u8) -> u8 {
    match depth {
        1 => (v * 255) as u8,
        2 => (v * 85) as u8,
        4 => (v * 17) as u8,
        8 => v as u8,
        _ => ((u32::from(v) * 255 + 32767) / 65535) as u8,
    }
}

fn pixel(h: &Header, row: &[u8], x: usize, palette: &[[u8; 3]], trns: &Trns) -> [u8; 4] {
    let d = h.depth;
    match h.color {
        0 => {
            let g = sample(row, d, x);
            let a = if trns.key.is_some_and(|k| k[0] == g) {
                0
            } else {
                255
            };
            let g = to8(g, d);
            [g, g, g, a]
        }
        2 => {
            let c = [
                sample(row, d, 3 * x),
                sample(row, d, 3 * x + 1),
                sample(row, d, 3 * x + 2),
            ];
            let a = if trns.key == Some(c) { 0 } else { 255 };
            [to8(c[0], d), to8(c[1], d), to8(c[2], d), a]
        }
        3 => {
            let i = sample(row, d, x) as usize;
            let [r, g, b] = palette.get(i).copied().unwrap_or([0, 0, 0]);
            let a = trns.alpha.get(i).copied().unwrap_or(255);
            [r, g, b, a]
        }
        4 => {
            let g = to8(sample(row, d, 2 * x), d);
            [g, g, g, to8(sample(row, d, 2 * x + 1), d)]
        }
        _ => [
            to8(sample(row, d, 4 * x), d),
            to8(sample(row, d, 4 * x + 1), d),
            to8(sample(row, d, 4 * x + 2), d),
            to8(sample(row, d, 4 * x + 3), d),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_what_it_encodes() {
        let pixels = [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 9, 9, 9, 255];
        let png = encode(2, 2, &pixels, ColorType::Rgba);
        let img = decode(&png).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.to_straight()[..4], [255, 0, 0, 255]);
        assert_eq!(img.pixel(1, 0), [0, 128, 0, 128]);
        assert_eq!(img.pixel(0, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn sub_byte_samples_unpack_from_the_high_bits() {
        let row = [0b1001_0011];
        assert_eq!(sample(&row, 1, 0), 1);
        assert_eq!(sample(&row, 1, 1), 0);
        assert_eq!(sample(&row, 2, 3), 3);
        assert_eq!(sample(&row, 4, 0), 9);
        assert_eq!(to8(3, 2), 255);
    }

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
