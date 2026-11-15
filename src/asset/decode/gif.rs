#[cfg(feature = "image-gif")]
use std::sync::Arc;
#[cfg(feature = "image-gif")]
use std::time::Duration;

#[cfg(feature = "image-gif")]
use crate::asset::types::DecodedGifFrame;
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

    let mut canvas = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut frames = Vec::new();
    let mut first_pixels: Option<Arc<[u8]>> = None;

    while let Some(frame) = decoder
        .read_next_frame()
        .map_err(|e| WaywiError::AssetDecode(format!("GIF frame decode failed: {e}")))?
    {
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

        let snapshot: Arc<[u8]> = Arc::from(canvas.as_slice());
        if first_pixels.is_none() {
            first_pixels = Some(Arc::clone(&snapshot));
        }
        let delay_ms = u64::from(frame.delay.max(2)) * 10;
        frames.push(DecodedGifFrame {
            width,
            height,
            pixels: snapshot,
            delay: Duration::from_millis(delay_ms),
        });

        if frame.dispose == gif::DisposalMethod::Background {
            canvas.fill(0);
        }
    }

    let primary = first_pixels
        .ok_or_else(|| WaywiError::AssetDecode("GIF contained no image frames".to_owned()))?;

    Ok(DecodedImage {
        width,
        height,
        pixels: primary,
        revision,
        animated_frames: if frames.len() > 1 {
            Some(Arc::new(frames))
        } else {
            None
        },
    })
}

#[cfg(not(feature = "image-gif"))]
pub(super) fn decode(_bytes: &[u8], _revision: u64) -> Result<DecodedImage> {
    Err(WaywiError::UnsupportedAssetFormat(
        "GIF support requires the `image-gif` feature".to_owned(),
    ))
}
