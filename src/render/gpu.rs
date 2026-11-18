//! Hardware-accelerated GPU renderer powered by `femtovg` + `wgpu`.

pub(crate) mod commands;
pub(crate) mod context;
pub(crate) mod textures;

use wayland_client::Connection;
use wayland_client::protocol::wl_surface::WlSurface;

use crate::capabilities::Capability;
use crate::error::{Result, WaywiError};
#[cfg(not(feature = "text"))]
use crate::geometry::LogicalPx;
use crate::geometry::{BufferPx, BufferRect, LogicalSize};
use crate::input::NodeId;
use crate::reconcile::RetainedSurfaceKey;
use crate::render::backend::{FrameScene, RenderBackend};
#[cfg(feature = "text")]
use crate::text::TextEngine;
use crate::text::{SharedStr, TextMeasureStyle};

use context::GpuContext;
use textures::GpuTextureCache;

/// GPU hardware vector renderer (`femtovg` + `wgpu`) implementing [`RenderBackend`].
pub(crate) struct GpuRenderer {
    ctx: Option<GpuContext>,
    textures: GpuTextureCache,
    #[cfg(feature = "text")]
    text_engine: TextEngine,
}

impl GpuRenderer {
    /// Probes for a usable GPU adapter and initializes the hardware renderer, returning `None`
    /// if no compatible Vulkan/GLES adapter is available.
    pub(crate) fn try_new(wayland_conn: Option<&Connection>) -> Option<Self> {
        let ctx = GpuContext::new(wayland_conn)?;
        Some(Self {
            ctx: Some(ctx),
            textures: GpuTextureCache::default(),
            #[cfg(feature = "text")]
            text_engine: TextEngine::new(),
        })
    }

    /// Creates a GPU renderer wrapper without failing when no GPU adapter exists (draws return
    /// [`WaywiError::UnsupportedCapability(Capability::GpuRenderer)`] until an adapter exists).
    pub(crate) fn new_with_probe(wayland_conn: Option<&Connection>) -> Self {
        Self::try_new(wayland_conn).unwrap_or_else(|| Self {
            ctx: None,
            textures: GpuTextureCache::default(),
            #[cfg(feature = "text")]
            text_engine: TextEngine::new(),
        })
    }

    pub(crate) fn is_available(&self) -> bool {
        self.ctx.is_some()
    }

    fn render_offscreen(
        &mut self,
        dst: &mut [u8],
        buf_w: BufferPx,
        buf_h: BufferPx,
        scene: FrameScene<'_>,
    ) -> Result<()> {
        let w = buf_w.as_u32();
        let h = buf_h.as_u32();
        if w == 0 || h == 0 {
            return Ok(());
        }
        let expected = (w as usize) * (h as usize) * 4;
        if dst.len() < expected {
            return Err(WaywiError::RenderError(format!(
                "destination buffer too small: {} < {expected}",
                dst.len()
            )));
        }

        let Some(ctx) = self.ctx.as_mut() else {
            return Err(WaywiError::UnsupportedCapability(Capability::GpuRenderer));
        };

        ctx.begin_frame(w, h, commands::to_femto_color(scene.background, 1.0));
        commands::encode_commands(
            &mut ctx.canvas,
            &mut self.textures,
            #[cfg(feature = "text")]
            &mut self.text_engine,
            scene,
        );
        ctx.flush_offscreen_pixels(w, h, dst)
    }
}

impl RenderBackend for GpuRenderer {
    fn drop_canvas_cache_for(&mut self, node_ids: &[NodeId]) {
        self.textures.drop_nodes(node_ids);
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

    fn render_argb8888(
        &mut self,
        dst_argb: &mut [u8],
        buf_w: BufferPx,
        buf_h: BufferPx,
        _dirty_rects: &[BufferRect],
        scene: FrameScene<'_>,
    ) -> Result<()> {
        self.render_offscreen(dst_argb, buf_w, buf_h, scene)
    }

    fn present_wayland_surface(
        &mut self,
        conn: &Connection,
        surface: &WlSurface,
        key: RetainedSurfaceKey,
        logical_size: LogicalSize,
        scene: FrameScene<'_>,
    ) -> Result<bool> {
        let Some(ctx) = self.ctx.as_mut() else {
            return Ok(false);
        };
        let (buf_w, buf_h) = logical_size.to_buffer_ceil(scene.scale);
        let w = buf_w.as_u32();
        let h = buf_h.as_u32();
        if !ctx.ensure_wayland_target(conn, surface, key, w, h) {
            return Ok(false);
        }
        ctx.begin_frame(w, h, commands::to_femto_color(scene.background, 1.0));
        commands::encode_commands(
            &mut ctx.canvas,
            &mut self.textures,
            #[cfg(feature = "text")]
            &mut self.text_engine,
            scene,
        );
        Ok(ctx.present_surface_frame(key))
    }

    fn drop_surface_target(&mut self, key: RetainedSurfaceKey) {
        if let Some(ctx) = self.ctx.as_mut() {
            ctx.drop_surface(key);
        }
    }
}
