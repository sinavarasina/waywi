//! Pure-Rust JPEG image decoder (`zune-jpeg` crate).

#[cfg(feature = "image-jpeg")]
use std::sync::Arc;

use crate::asset::types::DecodedImage;
use crate::error::{Result, WaywiError};

#[cfg(feature = "image-jpeg")]
use super::{premultiply_rgb_or_rgba, validate_dimensions};

#[cfg(feature = "image-jpeg")]
pub(super) fn decode(bytes: &[u8], revision: u64) -> Result<DecodedImage> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(16_384)
        .set_max_height(16_384);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|e| WaywiError::AssetDecode(format!("JPEG header decode failed: {e}")))?;

    let info = decoder
        .info()
        .ok_or_else(|| WaywiError::AssetDecode("JPEG image info missing".to_owned()))?;
    let width = u32::from(info.width);
    let height = u32::from(info.height);
    validate_dimensions(width, height)?;

    let raw = decoder
        .decode()
        .map_err(|e| WaywiError::AssetDecode(format!("JPEG decode failed: {e}")))?;
    let px_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| WaywiError::AssetDecode("JPEG pixel count overflow".to_owned()))?;

    let pixels: Vec<u8> = if raw.len() == px_count * 4 {
        // JPEG is always opaque (alpha == 255), so RGBA output is already premultiplied.
        raw
    } else if raw.len() == px_count * 3 {
        premultiply_rgb_or_rgba(&raw, width, height, false)?
    } else if raw.len() == px_count {
        let mut out = vec![0u8; px_count * 4];
        let (chunks, _) = out.as_chunks_mut::<4>();
        for (luma, dst) in raw.iter().copied().zip(chunks) {
            *dst = [luma, luma, luma, 255];
        }
        out
    } else {
        return Err(WaywiError::AssetDecode(format!(
            "unexpected JPEG decoded buffer length {} for {}x{}",
            raw.len(),
            width,
            height
        )));
    };

    Ok(DecodedImage {
        width,
        height,
        pixels: Arc::from(pixels),
        revision,
    })
}

#[cfg(not(feature = "image-jpeg"))]
pub(super) fn decode(_bytes: &[u8], _revision: u64) -> Result<DecodedImage> {
    Err(WaywiError::UnsupportedAssetFormat(
        "JPEG decoding requires the `image-jpeg` feature".to_owned(),
    ))
}

#[cfg(all(test, feature = "image-jpeg"))]
mod tests {
    use crate::asset::decode::{decode_image_bytes, detect_format};
    use crate::asset::types::ImageFormat;

    /// Valid minimal 1x1 baseline grayscale JPEG (white pixel).
    const TINY_WHITE_JPEG: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x03, 0x02, 0x02, 0x03, 0x02,
        0x02, 0x03, 0x03, 0x03, 0x03, 0x04, 0x03, 0x03, 0x04, 0x05, 0x08, 0x05, 0x05, 0x04, 0x04,
        0x05, 0x0A, 0x07, 0x07, 0x06, 0x08, 0x0C, 0x0A, 0x0C, 0x0C, 0x0B, 0x0A, 0x0B, 0x0B, 0x0D,
        0x0E, 0x12, 0x10, 0x0D, 0x0E, 0x11, 0x0E, 0x0B, 0x0B, 0x10, 0x16, 0x10, 0x11, 0x13, 0x14,
        0x15, 0x15, 0x15, 0x0C, 0x0F, 0x17, 0x18, 0x16, 0x14, 0x18, 0x12, 0x14, 0x15, 0x14, 0xFF,
        0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00,
        0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x08, 0xFF, 0xC4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x08,
        0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x7F, 0x00, 0xFF, 0xD9,
    ];

    #[test]
    fn detects_and_decodes_jpeg_to_opaque_rgba8() {
        let fmt = detect_format(TINY_WHITE_JPEG, None).expect("detect JPEG magic bytes");
        assert_eq!(fmt, ImageFormat::Jpeg);

        let decoded = decode_image_bytes(TINY_WHITE_JPEG, fmt, 1).expect("decode JPEG");
        assert_eq!(decoded.width, 1);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.pixels.len(), 4);
        assert_eq!(decoded.pixels[3], 255);
        assert_eq!(decoded.pixels[0], decoded.pixels[1]);
        assert_eq!(decoded.pixels[1], decoded.pixels[2]);
    }
}
