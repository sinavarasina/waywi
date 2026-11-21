use std::collections::HashMap;

use crate::error::{Result, WaywiError};
#[cfg(not(feature = "text"))]
use crate::geometry::LogicalPx;
use crate::geometry::{BufferPx, BufferRect, LogicalSize};
use crate::input::NodeId;
use crate::render::backend::{FrameScene, RenderBackend};
use crate::render::retained::PaintPayload;
use crate::render::software::fast_path::PixelTarget;
use crate::render::software::raster::TileTarget;
#[cfg(feature = "text")]
use crate::text::TextEngine;
use crate::text::{SharedStr, TextMeasureStyle};

pub(crate) mod bitmap;
pub(crate) mod fast_path;
pub(crate) mod path;
pub(crate) mod raster;

/// Upper bound retained on reusable software rasterization scratch buffers between frames
/// (2 MiB keeps panels/popups allocation-free while releasing temporary full-screen tiles).
const SCRATCH_RETAIN_MAX_BYTES: usize = 2 * 1024 * 1024;

/// Cached rasterized `Canvas` output keyed by `(NodeId, render_key, pixel_w, pixel_h)`.
pub(crate) struct CachedCanvasPixmap {
    pub(crate) render_key: u64,
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
    pub(crate) rgba_pixels: Vec<u8>,
}

/// Software CPU renderer powered by `tiny-skia` + axis-aligned fast paths + shared `TextEngine`.
pub(crate) struct SoftwareRenderer {
    #[cfg(feature = "text")]
    pub(crate) text_engine: TextEngine,
    pub(crate) canvas_cache: HashMap<NodeId, CachedCanvasPixmap>,
    pub(crate) scratch_rgba: Vec<u8>,
    pub(crate) scratch_sprite_cell: Vec<u8>,
}

impl Default for SoftwareRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftwareRenderer {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(feature = "text")]
            text_engine: TextEngine::new(),
            canvas_cache: HashMap::new(),
            scratch_rgba: Vec::new(),
            scratch_sprite_cell: Vec::new(),
        }
    }
}

impl RenderBackend for SoftwareRenderer {
    fn drop_canvas_cache_for(&mut self, node_ids: &[NodeId]) {
        for id in node_ids {
            self.canvas_cache.remove(id);
        }
    }

    fn measure_text(
        &mut self,
        content: &SharedStr,
        style: TextMeasureStyle<'_>,
        frame_seq: u64,
    ) -> LogicalSize {
        #[cfg(feature = "text")]
        {
            self.text_engine.measure_text(content, style, frame_seq)
        }
        #[cfg(not(feature = "text"))]
        {
            let _ = (
                style.line_height_multiplier,
                style.max_width,
                style.font,
                style.align,
                style.bounds_mode,
                frame_seq,
            );
            let char_w = style.font_size.0 * 0.6;
            LogicalSize::new(
                LogicalPx((content.len() as f32 * char_w).ceil()),
                LogicalPx((style.font_size.0 * 1.25).ceil()),
            )
        }
    }

    #[cfg(feature = "text")]
    fn measure_text_cursor(
        &mut self,
        content: &SharedStr,
        style: TextMeasureStyle<'_>,
        cursor_byte: usize,
        frame_seq: u64,
    ) -> crate::text::TextCursorMetrics {
        self.text_engine
            .measure_text_cursor(content, style, cursor_byte, frame_seq)
    }

    /// Renders `scene` across `dirty_rects` into a `wl_shm` little-endian `Argb8888`
    /// (`[B, G, R, A]`) buffer.
    fn render_argb8888(
        &mut self,
        dst_argb: &mut [u8],
        buf_w: BufferPx,
        buf_h: BufferPx,
        dirty_rects: &[BufferRect],
        scene: FrameScene<'_>,
    ) -> Result<()> {
        if !cfg!(feature = "renderer-software") {
            return Err(WaywiError::UnsupportedCapability(
                crate::capabilities::Capability::SoftwareRenderer,
            ));
        }
        let scale = scene.scale;
        let background = scene.background;
        let commands = scene.commands;
        let assets = scene.assets;
        let frame_seq = scene.frame_seq;

        let w_u32 = buf_w.as_u32();
        let h_u32 = buf_h.as_u32();
        if w_u32 == 0 || h_u32 == 0 {
            return Ok(());
        }
        let expected = (w_u32 as usize) * (h_u32 as usize) * 4;
        if dst_argb.len() < expected {
            return Err(WaywiError::RenderError(format!(
                "destination buffer too small: {} < {expected}",
                dst_argb.len()
            )));
        }

        let full_rect = BufferRect {
            x: 0,
            y: 0,
            width: buf_w.0,
            height: buf_h.0,
        };
        let regions: &[BufferRect] = if dirty_rects.is_empty() {
            std::slice::from_ref(&full_rect)
        } else {
            dirty_rects
        };

        for &dirty in regions {
            let Some(clipped) = dirty.intersect(full_rect) else {
                continue;
            };
            if clipped.is_empty() {
                continue;
            }
            let rw = clipped.width as u32;
            let rh = clipped.height as u32;
            let needed = (rw as usize) * (rh as usize) * 4;
            let mut scratch = std::mem::take(&mut self.scratch_rgba);
            if scratch.len() < needed {
                scratch.resize(needed, 0);
            }
            {
                let tile_rgba = &mut scratch[..needed];

                let bg_pm = background.to_premultiplied_rgba();
                if bg_pm == [0, 0, 0, 0] {
                    tile_rgba.fill(0);
                } else {
                    for px in tile_rgba.as_chunks_mut::<4>().0 {
                        px.copy_from_slice(&bg_pm);
                    }
                }

                let tile_local_clip = BufferRect {
                    x: 0,
                    y: 0,
                    width: rw as i32,
                    height: rh as i32,
                };

                let mut clip_stack: Vec<BufferRect> = Vec::new();
                for cmd in commands {
                    let cmd_buf = cmd.world_bounds.to_buffer_rect(scale, buf_w, buf_h);
                    if cmd_buf.intersect(clipped).is_none()
                        && !matches!(
                            cmd.payload,
                            PaintPayload::ClipPush { .. } | PaintPayload::ClipPop
                        )
                    {
                        continue;
                    }
                    match &cmd.payload {
                        PaintPayload::ClipPush { .. } => {
                            let c = cmd_buf.intersect(clipped).unwrap_or(BufferRect::default());
                            let shifted = BufferRect {
                                x: c.x - clipped.x,
                                y: c.y - clipped.y,
                                width: c.width,
                                height: c.height,
                            };
                            let prev = clip_stack.last().copied().unwrap_or(tile_local_clip);
                            clip_stack
                                .push(prev.intersect(shifted).unwrap_or(BufferRect::default()));
                        }
                        PaintPayload::ClipPop => {
                            clip_stack.pop();
                        }
                        _ => {
                            let active_clip = clip_stack.last().copied().unwrap_or(tile_local_clip);
                            if active_clip.is_empty() {
                                continue;
                            }
                            let mut tile = TileTarget {
                                pixels: PixelTarget {
                                    dst: tile_rgba,
                                    width: rw,
                                    height: rh,
                                    clip: active_clip,
                                },
                                origin_x: clipped.x,
                                origin_y: clipped.y,
                            };
                            self.render_command_into_tile(&mut tile, scale, cmd, assets, frame_seq);
                        }
                    }
                }
            }
            self.scratch_rgba = scratch;
            let tile_rgba = &self.scratch_rgba[..needed];

            // Copy & swizzle RGBA -> BGRA (`wl_shm::Format::Argb8888`) for the dirty tile only.
            for row in 0..(rh as usize) {
                let src_off = row * (rw as usize) * 4;
                let dst_y = (clipped.y as usize) + row;
                let dst_off = (dst_y * (w_u32 as usize) + (clipped.x as usize)) * 4;
                let src_row = &tile_rgba[src_off..src_off + (rw as usize) * 4];
                let dst_row = &mut dst_argb[dst_off..dst_off + (rw as usize) * 4];
                for (s, d) in src_row
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(dst_row.as_chunks_mut::<4>().0)
                {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = s[3];
                }
            }
        }

        if self.scratch_rgba.capacity() > SCRATCH_RETAIN_MAX_BYTES {
            self.scratch_rgba.clear();
            self.scratch_rgba.shrink_to(SCRATCH_RETAIN_MAX_BYTES);
        }
        if self.scratch_sprite_cell.capacity() > SCRATCH_RETAIN_MAX_BYTES {
            self.scratch_sprite_cell.clear();
            self.scratch_sprite_cell.shrink_to(SCRATCH_RETAIN_MAX_BYTES);
        }

        Ok(())
    }
}

#[cfg(all(test, feature = "renderer-software"))]
mod tests {
    use super::*;
    use crate::asset::AssetCache;
    use crate::geometry::ScaleFactor;
    use crate::render::Color;

    #[test]
    fn software_scratch_buffer_shrinks_after_large_fullscreen_tile() {
        let mut renderer = SoftwareRenderer::new();
        let assets = AssetCache::new();
        // 1920x400 RGBA = ~3.07 MiB (> 2 MiB retain cap).
        let (w, h) = (1920_i32, 400_i32);
        let mut dst = vec![0u8; (w as usize) * (h as usize) * 4];
        renderer
            .render_argb8888(
                &mut dst,
                BufferPx(w),
                BufferPx(h),
                &[],
                FrameScene {
                    scale: ScaleFactor::ONE,
                    background: Color::BLACK,
                    commands: &[],
                    assets: &assets,
                    frame_seq: 1,
                },
            )
            .unwrap();
        assert!(
            renderer.scratch_rgba.capacity() <= SCRATCH_RETAIN_MAX_BYTES,
            "expected scratch_rgba capacity <= {SCRATCH_RETAIN_MAX_BYTES}, got {}",
            renderer.scratch_rgba.capacity()
        );
    }
}
