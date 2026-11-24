#[cfg(feature = "image-svg")]
use std::sync::Arc;

use crate::asset::types::DecodedImage;
use crate::error::{Result, WaywiError};

#[cfg(feature = "image-svg")]
use super::validate_dimensions;

#[cfg(feature = "image-svg")]
pub(super) fn decode_min_size(bytes: &[u8], min_dim: u32, revision: u64) -> Result<DecodedImage> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &opt)
        .map_err(|e| WaywiError::AssetDecode(format!("SVG parse failed: {e}")))?;
    let size = tree.size();
    let intrinsic_w = size.width().max(1.0);
    let intrinsic_h = size.height().max(1.0);
    let max_side = intrinsic_w.max(intrinsic_h);
    let scale = if min_dim > 0 && max_side < min_dim as f32 {
        min_dim as f32 / max_side
    } else {
        1.0
    };
    let (width, height, transform) = if scale > 1.0 {
        (
            (intrinsic_w * scale).round().max(1.0) as u32,
            (intrinsic_h * scale).round().max(1.0) as u32,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
        )
    } else {
        let int_size = size.to_int_size();
        (
            int_size.width().max(1),
            int_size.height().max(1),
            resvg::tiny_skia::Transform::default(),
        )
    };
    validate_dimensions(width, height)?;

    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| WaywiError::AssetDecode("failed to allocate SVG pixmap".to_owned()))?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Ok(DecodedImage {
        width,
        height,
        pixels: Arc::from(pixmap.take()),
        revision,
    })
}

#[cfg(not(feature = "image-svg"))]
pub(super) fn decode_min_size(
    _bytes: &[u8],
    _min_dim: u32,
    _revision: u64,
) -> Result<DecodedImage> {
    Err(WaywiError::UnsupportedAssetFormat(
        "SVG support requires the `image-svg` feature".to_owned(),
    ))
}
