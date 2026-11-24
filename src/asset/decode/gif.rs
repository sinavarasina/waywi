#[cfg(feature = "image-gif")]
use std::sync::Arc;

use crate::asset::types::DecodedImage;
use crate::error::{Result, WaywiError};

#[cfg(feature = "image-gif")]
use super::{premultiply_into, validate_dimensions};

#[cfg(feature = "image-gif")]
pub(super) fn decode(bytes: &[u8], revision: u64) -> Result<DecodedImage> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options
        .read_info(std::io::Cursor::new(bytes))
        .map_err(|e| WaywiError::AssetDecode(format!("GIF decode failed: {e}")))?;
    let width = u32::from(decoder.width());
    let height = u32::from(decoder.height());
    validate_dimensions(width, height)?;

    let frame = decoder
        .read_next_frame()
        .map_err(|e| WaywiError::AssetDecode(format!("GIF frame decode failed: {e}")))?
        .ok_or_else(|| WaywiError::AssetDecode("GIF contained no image frames".to_owned()))?;

    let mut canvas = vec![0u8; (width as usize) * (height as usize) * 4];
    let fx = usize::from(frame.left);
    let fy = usize::from(frame.top);
    let fw = usize::from(frame.width);
    let fh = usize::from(frame.height);

    for row in 0..fh {
        let cy = fy + row;
        if cy >= height as usize {
            continue;
        }
        for col in 0..fw {
            let cx = fx + col;
            if cx >= width as usize {
                continue;
            }
            let src_idx = (row * fw + col) * 4;
            let dst_idx = (cy * (width as usize) + cx) * 4;
            if src_idx + 3 < frame.buffer.len() {
                let a = frame.buffer[src_idx + 3];
                if a > 0 {
                    premultiply_into(
                        &mut canvas[dst_idx..dst_idx + 4],
                        frame.buffer[src_idx],
                        frame.buffer[src_idx + 1],
                        frame.buffer[src_idx + 2],
                        a,
                    );
                }
            }
        }
    }

    Ok(DecodedImage {
        width,
        height,
        pixels: Arc::from(canvas),
        revision,
    })
}

#[cfg(not(feature = "image-gif"))]
pub(super) fn decode(_bytes: &[u8], _revision: u64) -> Result<DecodedImage> {
    Err(WaywiError::UnsupportedAssetFormat(
        "GIF support requires the `image-gif` feature".to_owned(),
    ))
}
