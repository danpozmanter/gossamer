//! In-memory raster images plus PNG and JPEG codecs.
//!
//! [`Image`] uses a canonical, row-major RGBA8 backing store. PNG round trips
//! alpha exactly. JPEG has no alpha channel, so encoding composites transparent
//! pixels against black and decoding always returns opaque pixels. Keeping one
//! model avoids format-specific colour and stride surprises at API boundaries.

#![forbid(unsafe_code)]

use std::io::Cursor;

/// One straight-alpha sRGBA pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgba {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel.
    pub a: u8,
}

impl Rgba {
    /// Opaque black.
    pub const BLACK: Self = Self::new(0, 0, 0, 255);
    /// Opaque white.
    pub const WHITE: Self = Self::new(255, 255, 255, 255);
    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);

    /// Constructs a pixel from its four channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// Owned row-major RGBA8 raster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    width: u32,
    height: u32,
    pixels: Vec<Rgba>,
}

impl Image {
    /// Allocates a transparent image.
    ///
    /// Returns [`ImageError::Dimensions`] if `width * height` cannot fit in
    /// memory on this platform.
    pub fn new(width: u32, height: u32) -> Result<Self, ImageError> {
        Self::filled(width, height, Rgba::TRANSPARENT)
    }

    /// Allocates an image whose pixels all equal `color`.
    pub fn filled(width: u32, height: u32, color: Rgba) -> Result<Self, ImageError> {
        let count = pixel_count(width, height)?;
        Ok(Self {
            width,
            height,
            pixels: vec![color; count],
        })
    }

    /// Constructs an image from exact row-major RGBA bytes.
    pub fn from_rgba8(width: u32, height: u32, bytes: Vec<u8>) -> Result<Self, ImageError> {
        let count = pixel_count(width, height)?;
        let expected = count.checked_mul(4).ok_or(ImageError::Dimensions)?;
        if bytes.len() != expected {
            return Err(ImageError::InvalidBuffer {
                expected,
                actual: bytes.len(),
            });
        }
        let pixels = bytes
            .chunks_exact(4)
            .map(|pixel| Rgba::new(pixel[0], pixel[1], pixel[2], pixel[3]))
            .collect();
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Pixel width.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Pixel height.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Returns the pixel at `(x, y)`, or `None` outside the image.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgba> {
        self.index(x, y).map(|index| self.pixels[index])
    }

    /// Replaces the pixel at `(x, y)`. Returns `false` outside the image.
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Rgba) -> bool {
        let Some(index) = self.index(x, y) else {
            return false;
        };
        self.pixels[index] = color;
        true
    }

    /// Returns a new row-major RGBA8 byte buffer.
    #[must_use]
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.pixels.len() * 4);
        for pixel in &self.pixels {
            bytes.extend_from_slice(&[pixel.r, pixel.g, pixel.b, pixel.a]);
        }
        bytes
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        usize::try_from(u64::from(y) * u64::from(self.width) + u64::from(x)).ok()
    }
}

/// Image operation failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageError {
    /// Dimensions cannot be represented as an in-memory buffer.
    #[error("image: dimensions are too large")]
    Dimensions,
    /// Raw RGBA input does not have the exact required length.
    #[error("image: invalid RGBA buffer length: expected {expected}, got {actual}")]
    InvalidBuffer {
        /// Required RGBA byte count.
        expected: usize,
        /// Bytes supplied by the caller.
        actual: usize,
    },
    /// Input has no supported PNG or JPEG signature.
    #[error("image: unsupported format")]
    UnsupportedFormat,
    /// PNG codec failure.
    #[error("image: png: {0}")]
    Png(String),
    /// JPEG codec failure.
    #[error("image: jpeg: {0}")]
    Jpeg(String),
}

/// Decodes a PNG or JPEG image, selected from its wire signature.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes)
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        decode_jpeg(bytes)
    } else {
        Err(ImageError::UnsupportedFormat)
    }
}

/// Encodes an image as a lossless RGBA PNG.
pub fn encode_png(image: &Image) -> Result<Vec<u8>, ImageError> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| ImageError::Png(error.to_string()))?;
    writer
        .write_image_data(&image.to_rgba8())
        .map_err(|error| ImageError::Png(error.to_string()))?;
    drop(writer);
    Ok(out)
}

/// Decodes a PNG into the canonical RGBA8 model.
pub fn decode_png(bytes: &[u8]) -> Result<Image, ImageError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| ImageError::Png(error.to_string()))?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| ImageError::Png(error.to_string()))?;
    let bytes = &buffer[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        png::ColorType::Grayscale => bytes
            .iter()
            .flat_map(|&value| [value, value, value, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => bytes
            .chunks_exact(2)
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        png::ColorType::Indexed => {
            return Err(ImageError::Png("indexed output was not expanded".into()));
        }
    };
    Image::from_rgba8(info.width, info.height, rgba)
}

/// Encodes an image as a JPEG with `quality` clamped to `1..=100`.
///
/// JPEG does not represent alpha. Transparent pixels are composited against
/// black using integer straight-alpha blending before encoding.
pub fn encode_jpeg(image: &Image, quality: u8) -> Result<Vec<u8>, ImageError> {
    let mut rgb = Vec::with_capacity(image.pixels.len() * 3);
    for pixel in &image.pixels {
        let alpha = u16::from(pixel.a);
        rgb.extend([
            ((u16::from(pixel.r) * alpha + 127) / 255) as u8,
            ((u16::from(pixel.g) * alpha + 127) / 255) as u8,
            ((u16::from(pixel.b) * alpha + 127) / 255) as u8,
        ]);
    }
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, quality.clamp(1, 100));
    encoder
        .encode(
            &rgb,
            u16::try_from(image.width).map_err(|_| ImageError::Dimensions)?,
            u16::try_from(image.height).map_err(|_| ImageError::Dimensions)?,
            jpeg_encoder::ColorType::Rgb,
        )
        .map_err(|error| ImageError::Jpeg(error.to_string()))?;
    Ok(out)
}

/// Decodes a JPEG into the canonical opaque RGBA8 model.
pub fn decode_jpeg(bytes: &[u8]) -> Result<Image, ImageError> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
    let data = decoder
        .decode()
        .map_err(|error| ImageError::Jpeg(error.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| ImageError::Jpeg("missing image metadata".into()))?;
    let channels = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => 1,
        jpeg_decoder::PixelFormat::RGB24 => 3,
        jpeg_decoder::PixelFormat::CMYK32 => 4,
        jpeg_decoder::PixelFormat::L16 => {
            return Err(ImageError::Jpeg("16-bit grayscale is unsupported".into()));
        }
    };
    let expected = pixel_count(u32::from(info.width), u32::from(info.height))
        .and_then(|count| count.checked_mul(channels).ok_or(ImageError::Dimensions))?;
    if data.len() != expected {
        return Err(ImageError::Jpeg(
            "decoded buffer has an invalid length".into(),
        ));
    }
    let rgba = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => data
            .iter()
            .flat_map(|&value| [value, value, value, 255])
            .collect(),
        jpeg_decoder::PixelFormat::RGB24 => data
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        jpeg_decoder::PixelFormat::CMYK32 => data
            .chunks_exact(4)
            .flat_map(|pixel| {
                let c = u16::from(pixel[0]);
                let m = u16::from(pixel[1]);
                let y = u16::from(pixel[2]);
                let k = u16::from(pixel[3]);
                [
                    (255 - ((c + k).min(255))) as u8,
                    (255 - ((m + k).min(255))) as u8,
                    (255 - ((y + k).min(255))) as u8,
                    255,
                ]
            })
            .collect(),
        jpeg_decoder::PixelFormat::L16 => unreachable!("pixel format was checked above"),
    };
    Image::from_rgba8(u32::from(info.width), u32::from(info.height), rgba)
}

fn pixel_count(width: u32, height: u32) -> Result<usize, ImageError> {
    usize::try_from(u64::from(width) * u64::from(height)).map_err(|_| ImageError::Dimensions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip_preserves_rgba_and_dimensions() {
        let mut image = Image::filled(2, 1, Rgba::WHITE).unwrap();
        assert!(image.set_pixel(1, 0, Rgba::new(7, 8, 9, 10)));
        let encoded = encode_png(&image).unwrap();
        assert!(encoded.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(decode(&encoded).unwrap(), image);
    }

    #[test]
    fn jpeg_round_trip_is_opaque_and_near_source_colour() {
        let image = Image::filled(1, 1, Rgba::new(240, 20, 10, 255)).unwrap();
        let decoded = decode_jpeg(&encode_jpeg(&image, 100).unwrap()).unwrap();
        let pixel = decoded.pixel(0, 0).unwrap();
        assert_eq!(pixel.a, 255);
        assert!(
            pixel.r >= 230 && pixel.g <= 30 && pixel.b <= 20,
            "{pixel:?}"
        );
    }

    #[test]
    fn jpeg_composites_transparency_against_black() {
        let image = Image::filled(1, 1, Rgba::new(200, 100, 50, 0)).unwrap();
        let decoded = decode_jpeg(&encode_jpeg(&image, 100).unwrap()).unwrap();
        let pixel = decoded.pixel(0, 0).unwrap();
        assert!(pixel.r <= 8 && pixel.g <= 8 && pixel.b <= 8, "{pixel:?}");
    }

    #[test]
    fn malformed_and_unknown_input_are_errors() {
        assert_eq!(decode(b"not an image"), Err(ImageError::UnsupportedFormat));
        assert!(decode_png(b"\x89PNG\r\n\x1a\nnope").is_err());
        assert!(decode_jpeg(&[0xff, 0xd8, 0xff]).is_err());
    }

    #[test]
    fn raw_buffer_and_bounds_are_checked() {
        assert!(matches!(
            Image::from_rgba8(1, 1, vec![0; 3]),
            Err(ImageError::InvalidBuffer { .. })
        ));
        let mut image = Image::new(1, 1).unwrap();
        assert!(!image.set_pixel(1, 0, Rgba::WHITE));
        assert_eq!(image.pixel(1, 0), None);
    }
}
