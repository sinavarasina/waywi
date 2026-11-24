//! Bounded image loading, format sniffing, and per-format decoder dispatch.

mod gif;
mod png;
mod svg;
mod webp;

use std::hash::{Hash, Hasher};
use std::path::Path;

use crate::error::{Result, WaywiError};

use super::types::{DecodedImage, ImageFormat};

/// Maximum image file read size (64 MiB) to protect against malformed inputs.
const MAX_ASSET_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum raster image width/height (16,384 px).
const MAX_IMAGE_DIMENSION: u32 = 16_384;

pub(crate) fn read_bounded_file(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_ASSET_FILE_BYTES {
        return Err(WaywiError::AssetDecode(format!(
            "asset {} exceeds max size ({} > {} bytes)",
            path.display(),
            meta.len(),
            MAX_ASSET_FILE_BYTES
        )));
    }
    Ok(std::fs::read(path)?)
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.len().hash(&mut hasher);
    bytes.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn detect_format(bytes: &[u8], path: Option<&Path>) -> Result<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(ImageFormat::Png);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok(ImageFormat::Gif);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok(ImageFormat::Webp);
    }
    let trimmed = bytes
        .iter()
        .copied()
        .skip_while(|b| b.is_ascii_whitespace())
        .take(256)
        .collect::<Vec<u8>>();
    if let Ok(head) = std::str::from_utf8(&trimmed) {
        let lower = head.to_ascii_lowercase();
        if lower.contains("<svg") || (lower.starts_with("<?xml") && lower.contains("svg")) {
            return Ok(ImageFormat::Svg);
        }
    }
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        match ext.to_ascii_lowercase().as_str() {
            "png" => return Ok(ImageFormat::Png),
            "svg" | "svgz" => return Ok(ImageFormat::Svg),
            "gif" => return Ok(ImageFormat::Gif),
            "webp" => return Ok(ImageFormat::Webp),
            other => {
                return Err(WaywiError::UnsupportedAssetFormat(format!(
                    "unsupported image extension .{other}"
                )));
            }
        }
    }
    Err(WaywiError::UnsupportedAssetFormat(
        "unrecognized image format".to_owned(),
    ))
}

pub(crate) fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(WaywiError::AssetDecode(
            "image dimensions must be non-zero".to_owned(),
        ));
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(WaywiError::AssetDecode(format!(
            "image dimensions {width}x{height} exceed safety limit {MAX_IMAGE_DIMENSION}"
        )));
    }
    Ok(())
}

pub(crate) fn decode_image_bytes(
    bytes: &[u8],
    format: ImageFormat,
    revision: u64,
) -> Result<DecodedImage> {
    decode_image_bytes_sized(bytes, format, 0, revision)
}

pub(crate) fn decode_image_bytes_sized(
    bytes: &[u8],
    format: ImageFormat,
    min_svg_px: u32,
    revision: u64,
) -> Result<DecodedImage> {
    match format {
        ImageFormat::Png => png::decode(bytes, revision),
        ImageFormat::Svg => svg::decode_min_size(bytes, min_svg_px, revision),
        ImageFormat::Gif => gif::decode(bytes, revision),
        ImageFormat::Webp => webp::decode(bytes, revision),
    }
}

/// Converts packed straight RGBA8 or RGB8 rows into premultiplied RGBA8 pixels.
#[cfg(feature = "image-webp")]
pub(crate) fn premultiply_rgb_or_rgba(
    src: &[u8],
    width: u32,
    height: u32,
    has_alpha: bool,
) -> Result<Vec<u8>> {
    let px_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| WaywiError::AssetDecode("image pixel count overflow".to_owned()))?;
    let expected_src = px_count
        .checked_mul(if has_alpha { 4 } else { 3 })
        .ok_or_else(|| WaywiError::AssetDecode("image buffer size overflow".to_owned()))?;
    if src.len() < expected_src {
        return Err(WaywiError::AssetDecode(format!(
            "decoded pixel buffer too small ({} < {expected_src} bytes)",
            src.len()
        )));
    }
    let mut out = vec![0u8; px_count * 4];
    if has_alpha {
        for (src_px, dst_px) in src[..expected_src]
            .as_chunks::<4>()
            .0
            .iter()
            .zip(out.as_chunks_mut::<4>().0.iter_mut())
        {
            premultiply_into(dst_px, src_px[0], src_px[1], src_px[2], src_px[3]);
        }
    } else {
        for (src_px, dst_px) in src[..expected_src]
            .as_chunks::<3>()
            .0
            .iter()
            .zip(out.as_chunks_mut::<4>().0.iter_mut())
        {
            *dst_px = [src_px[0], src_px[1], src_px[2], 255];
        }
    }
    Ok(out)
}

#[inline]
pub(crate) fn premultiply_into(dst: &mut [u8], r: u8, g: u8, b: u8, a: u8) {
    if a == 255 {
        dst[0] = r;
        dst[1] = g;
        dst[2] = b;
        dst[3] = 255;
    } else if a == 0 {
        dst[0] = 0;
        dst[1] = 0;
        dst[2] = 0;
        dst[3] = 0;
    } else {
        let aa = u16::from(a);
        dst[0] = ((u16::from(r) * aa + 127) / 255) as u8;
        dst[1] = ((u16::from(g) * aa + 127) / 255) as u8;
        dst[2] = ((u16::from(b) * aa + 127) / 255) as u8;
        dst[3] = a;
    }
}
