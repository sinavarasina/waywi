//! Pure-Rust WebP image decoder (`image-webp` crate).

#[cfg(feature = "image-webp")]
use std::sync::Arc;

use crate::asset::types::DecodedImage;
use crate::error::{Result, WaywiError};

#[cfg(feature = "image-webp")]
use super::{premultiply_rgb_or_rgba, validate_dimensions};

#[cfg(feature = "image-webp")]
const WEBP_DECODER_MEM_LIMIT: usize = 64 * 1024 * 1024;

#[cfg(feature = "image-webp")]
pub(super) fn decode(bytes: &[u8], revision: u64) -> Result<DecodedImage> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| WaywiError::AssetDecode(format!("WebP header decode failed: {e}")))?;
    decoder.set_memory_limit(WEBP_DECODER_MEM_LIMIT);
    let (width, height) = decoder.dimensions();
    validate_dimensions(width, height)?;

    let has_alpha = decoder.has_alpha();
    let buf_size = decoder
        .output_buffer_size()
        .ok_or_else(|| WaywiError::AssetDecode("WebP output buffer size overflow".to_owned()))?;
    let mut raw = vec![0u8; buf_size];

    if decoder.is_animated() {
        decoder.read_frame(&mut raw).map_err(|e| match e {
            image_webp::DecodingError::NoMoreFrames => {
                WaywiError::AssetDecode("WebP contained no image frames".to_owned())
            }
            other => {
                WaywiError::AssetDecode(format!("WebP animation frame decode failed: {other}"))
            }
        })?;
    } else {
        decoder
            .read_image(&mut raw)
            .map_err(|e| WaywiError::AssetDecode(format!("WebP decode failed: {e}")))?;
    }

    let premul = premultiply_rgb_or_rgba(&raw, width, height, has_alpha)?;
    Ok(DecodedImage {
        width,
        height,
        pixels: Arc::from(premul),
        revision,
    })
}

#[cfg(not(feature = "image-webp"))]
pub(super) fn decode(_bytes: &[u8], _revision: u64) -> Result<DecodedImage> {
    Err(WaywiError::UnsupportedAssetFormat(
        "WebP decoding requires the `image-webp` feature".to_owned(),
    ))
}

#[cfg(all(test, feature = "image-webp"))]
mod tests {
    use crate::asset::decode::{decode_image_bytes, detect_format};
    use crate::asset::types::ImageFormat;

    #[test]
    fn decodes_lossless_rgba_webp_into_premultiplied_pixels() {
        let mut webp_bytes = Vec::new();
        let encoder = image_webp::WebPEncoder::new(&mut webp_bytes);
        // 2x2 RGBA: semi-transparent red + opaque blue + transparent + white
        let pixels: [u8; 16] = [
            200, 0, 0, 128, // premultiplied -> ~100, 0, 0, 128
            0, 40, 255, 255, // opaque
            255, 255, 255, 0, // fully transparent -> 0, 0, 0, 0
            255, 255, 255, 255,
        ];
        encoder
            .encode(&pixels, 2, 2, image_webp::ColorType::Rgba8)
            .expect("encode test webp");

        let fmt = detect_format(&webp_bytes, None).expect("detect RIFF WEBP");
        assert_eq!(fmt, ImageFormat::Webp);

        let decoded = decode_image_bytes(&webp_bytes, fmt, 1).expect("decode webp");
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 2);
        // Pixel 0 premultiplied: r ≈ 100
        assert!((decoded.pixels[0] as i32 - 100).abs() <= 1);
        assert_eq!(decoded.pixels[3], 128);
        // Pixel 2 fully transparent -> premultiplied 0
        assert_eq!(&decoded.pixels[8..12], &[0, 0, 0, 0]);
    }
}
