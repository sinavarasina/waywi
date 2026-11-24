#[cfg(feature = "image-png")]
use std::sync::Arc;

use crate::asset::types::DecodedImage;
use crate::error::{Result, WaywiError};

#[cfg(feature = "image-png")]
use super::{premultiply_into, validate_dimensions};

#[cfg(feature = "image-png")]
pub(super) fn decode(bytes: &[u8], revision: u64) -> Result<DecodedImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| WaywiError::AssetDecode(format!("PNG header decode failed: {e}")))?;
    let info = reader.info();
    let width = info.width;
    let height = info.height;
    validate_dimensions(width, height)?;

    let buf_size = reader
        .output_buffer_size()
        .ok_or_else(|| WaywiError::AssetDecode("PNG output buffer size overflow".to_owned()))?;
    let mut raw = vec![0u8; buf_size];
    let frame_info = reader
        .next_frame(&mut raw)
        .map_err(|e| WaywiError::AssetDecode(format!("PNG frame decode failed: {e}")))?;
    let src = &raw[..frame_info.buffer_size()];

    let px_count = (width as usize) * (height as usize);
    let mut out = vec![0u8; px_count * 4];
    match frame_info.color_type {
        png::ColorType::Rgba => {
            for i in 0..px_count {
                let s = i * 4;
                premultiply_into(
                    &mut out[s..s + 4],
                    src[s],
                    src[s + 1],
                    src[s + 2],
                    src[s + 3],
                );
            }
        }
        png::ColorType::Rgb => {
            for i in 0..px_count {
                let s = i * 3;
                let d = i * 4;
                out[d] = src[s];
                out[d + 1] = src[s + 1];
                out[d + 2] = src[s + 2];
                out[d + 3] = 255;
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for i in 0..px_count {
                let s = i * 2;
                let d = i * 4;
                premultiply_into(&mut out[d..d + 4], src[s], src[s], src[s], src[s + 1]);
            }
        }
        png::ColorType::Grayscale => {
            for (&g, dst) in src
                .iter()
                .take(px_count)
                .zip(out.as_chunks_mut::<4>().0.iter_mut())
            {
                *dst = [g, g, g, 255];
            }
        }
        png::ColorType::Indexed => {
            return Err(WaywiError::AssetDecode(
                "unexpected indexed PNG after EXPAND transformation".to_owned(),
            ));
        }
    }

    Ok(DecodedImage {
        width,
        height,
        pixels: Arc::from(out),
        revision,
    })
}

#[cfg(not(feature = "image-png"))]
pub(super) fn decode(_bytes: &[u8], _revision: u64) -> Result<DecodedImage> {
    Err(WaywiError::UnsupportedAssetFormat(
        "PNG support requires the `image-png` feature".to_owned(),
    ))
}
