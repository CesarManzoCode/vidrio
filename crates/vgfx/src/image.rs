//! Decoded raster images: RGBA8 with premultiplied alpha, the format
//! sniffing that picks a decoder (PNG, GIF, JPEG) and the two samplers
//! of painting: bilinear (the default, `image-rendering: auto`) and
//! nearest neighbor (`pixelated` and `crisp-edges`).

use std::fmt;

/// A decoded image, rows top to bottom, premultiplied RGBA8.
#[derive(Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Image({}×{})", self.width, self.height)
    }
}

/// Why an image did not decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageError(pub String);

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub(crate) fn err<T>(m: &str) -> Result<T, ImageError> {
    Err(ImageError(m.to_string()))
}

/// The largest width or height accepted (a bound on memory).
pub const MAX_SIDE: u32 = 16384;

/// The image formats vgfx decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Gif,
    Jpeg,
}

/// The format of `bytes` from their signature (WHATWG MIME Sniffing
/// §7.1).
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else {
        None
    }
}

/// Decodes a PNG (the first frame of an APNG), a GIF (its first frame)
/// or a JPEG.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
    match sniff(bytes) {
        Some(Format::Png) => crate::png::decode(bytes),
        Some(Format::Gif) => crate::gif::decode(bytes),
        Some(Format::Jpeg) => crate::jpeg::decode(bytes),
        None => err("unknown image format"),
    }
}

/// Multiplies straight color by alpha, rounding to nearest.
pub fn premultiply(rgba: [u8; 4]) -> [u8; 4] {
    let a = u32::from(rgba[3]);
    if a == 255 {
        return rgba;
    }
    let m = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
    [m(rgba[0]), m(rgba[1]), m(rgba[2]), rgba[3]]
}

/// Divides premultiplied color by alpha.
pub fn unpremultiply(p: [u8; 4]) -> [u8; 4] {
    let a = u32::from(p[3]);
    match a {
        0 => [0, 0, 0, 0],
        255 => p,
        _ => {
            let d = |c: u8| ((u32::from(c) * 255 + a / 2) / a).min(255) as u8;
            [d(p[0]), d(p[1]), d(p[2]), p[3]]
        }
    }
}

impl Image {
    /// An image from straight-alpha RGBA8 pixels.
    pub fn from_straight(width: u32, height: u32, mut data: Vec<u8>) -> Image {
        for p in data.chunks_exact_mut(4) {
            let q = premultiply([p[0], p[1], p[2], p[3]]);
            p.copy_from_slice(&q);
        }
        Image {
            width,
            height,
            data,
        }
    }

    /// A transparent image.
    pub fn transparent(width: u32, height: u32) -> Image {
        Image {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
        }
    }

    /// The premultiplied pixel at (`x`, `y`).
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// The pixels with straight alpha.
    pub fn to_straight(&self) -> Vec<u8> {
        let mut out = self.data.clone();
        for p in out.chunks_exact_mut(4) {
            let q = unpremultiply([p[0], p[1], p[2], p[3]]);
            p.copy_from_slice(&q);
        }
        out
    }

    /// Samples at (`u`, `v`) in image pixels (pixel centers at .5) with
    /// bilinear filtering; texels outside the source rectangle `src`
    /// (x0, y0, x1, y1, in whole pixels) repeat its edge.
    pub fn sample_bilinear(&self, u: f32, v: f32, src: (u32, u32, u32, u32)) -> [u8; 4] {
        let (sx0, sy0, sx1, sy1) = src;
        if sx1 <= sx0 || sy1 <= sy0 {
            return [0; 4];
        }
        let fx = u - 0.5;
        let fy = v - 0.5;
        let x0 = fx.floor();
        let y0 = fy.floor();
        let ax = fx - x0;
        let ay = fy - y0;
        let cx = |x: f32| (x as i64).clamp(sx0 as i64, sx1 as i64 - 1) as u32;
        let cy = |y: f32| (y as i64).clamp(sy0 as i64, sy1 as i64 - 1) as u32;
        let (xa, xb, ya, yb) = (cx(x0), cx(x0 + 1.0), cy(y0), cy(y0 + 1.0));
        if ax == 0.0 && ay == 0.0 {
            return self.pixel(xa, ya);
        }
        let p00 = self.pixel(xa, ya);
        let p10 = self.pixel(xb, ya);
        let p01 = self.pixel(xa, yb);
        let p11 = self.pixel(xb, yb);
        let mut out = [0u8; 4];
        for k in 0..4 {
            let top = f32::from(p00[k]) * (1.0 - ax) + f32::from(p10[k]) * ax;
            let bottom = f32::from(p01[k]) * (1.0 - ax) + f32::from(p11[k]) * ax;
            out[k] = (top * (1.0 - ay) + bottom * ay + 0.5).min(255.0) as u8;
        }
        out
    }

    /// Samples the texel that contains (`u`, `v`), clamped to `src`.
    pub fn sample_nearest(&self, u: f32, v: f32, src: (u32, u32, u32, u32)) -> [u8; 4] {
        let (sx0, sy0, sx1, sy1) = src;
        if sx1 <= sx0 || sy1 <= sy0 {
            return [0; 4];
        }
        let x = (u.floor() as i64).clamp(sx0 as i64, sx1 as i64 - 1) as u32;
        let y = (v.floor() as i64).clamp(sy0 as i64, sy1 as i64 - 1) as u32;
        self.pixel(x, y)
    }

    /// The image scaled to `width` × `height` with bilinear filtering.
    pub fn scaled_bilinear(&self, width: u32, height: u32) -> Image {
        let mut out = Image::transparent(width, height);
        if self.width == 0 || self.height == 0 {
            return out;
        }
        let sx = self.width as f32 / width.max(1) as f32;
        let sy = self.height as f32 / height.max(1) as f32;
        let src = (0, 0, self.width, self.height);
        for y in 0..height {
            for x in 0..width {
                let p = self.sample_bilinear((x as f32 + 0.5) * sx, (y as f32 + 0.5) * sy, src);
                let i = (y as usize * width as usize + x as usize) * 4;
                out.data[i..i + 4].copy_from_slice(&p);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplication_round_trips_opaque_and_transparent() {
        assert_eq!(premultiply([200, 100, 50, 255]), [200, 100, 50, 255]);
        assert_eq!(premultiply([200, 100, 50, 0]), [0, 0, 0, 0]);
        assert_eq!(premultiply([255, 0, 0, 128]), [128, 0, 0, 128]);
        assert_eq!(unpremultiply([128, 0, 0, 128]), [255, 0, 0, 128]);
    }

    #[test]
    fn bilinear_at_texel_centers_is_exact_and_blends_between() {
        let img = Image::from_straight(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255]);
        let src = (0, 0, 2, 1);
        assert_eq!(img.sample_bilinear(0.5, 0.5, src), [0, 0, 0, 255]);
        assert_eq!(img.sample_bilinear(1.5, 0.5, src), [255, 255, 255, 255]);
        assert_eq!(img.sample_bilinear(1.0, 0.5, src), [128, 128, 128, 255]);
        assert_eq!(img.sample_nearest(0.9, 0.5, src), [0, 0, 0, 255]);
    }

    #[test]
    fn sniffs_signatures() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxx"), Some(Format::Png));
        assert_eq!(sniff(b"GIF89a.."), Some(Format::Gif));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(sniff(b"BM"), None);
    }
}
