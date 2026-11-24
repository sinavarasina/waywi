//! Hardware-accelerated GPU renderer powered by `femtovg` + `wgpu`.

pub(crate) mod commands;
pub(crate) mod context;
pub(crate) mod textures;

use wayland_client::Connection;
use wayland_client::protocol::wl_surface::WlSurface;

use crate::asset::AssetId;
use crate::error::{Result, WaywiError};
use crate::geometry::{BufferPx, LogicalSize};
use crate::input::NodeId;
use crate::reconcile::RetainedSurfaceKey;
use crate::render::backend::FrameScene;
#[cfg(feature = "text")]
use crate::text::TextEngine;

use context::GpuContext;
use textures::GpuTextureCache;

/// GPU hardware vector renderer (`femtovg` + `wgpu`).
pub(crate) struct GpuRenderer {
    ctx: GpuContext,
    textures: GpuTextureCache,
}

impl GpuRenderer {
    /// Probes for a usable GPU adapter and initializes the hardware renderer, returning `None`
    /// if no compatible Vulkan/GLES adapter is available.
    pub(crate) fn try_new(
        wayland_conn: Option<&Connection>,
        cache_max_bytes: usize,
        stale_age_frames: u64,
    ) -> Option<Self> {
        let ctx = GpuContext::new(wayland_conn)?;
        Some(Self {
            ctx,
            textures: GpuTextureCache::with_limits(cache_max_bytes, stale_age_frames),
        })
    }

    pub(crate) fn drop_canvas_cache_for(&mut self, node_ids: &[NodeId]) {
        self.textures.drop_nodes(node_ids);
    }

    pub(crate) fn evict_asset(&mut self, asset_id: AssetId) {
        self.textures.evict_asset(&mut self.ctx.canvas, asset_id);
    }

    pub(crate) fn trim_memory(&mut self) {
        self.textures.trim(&mut self.ctx.canvas);
        self.ctx.trim_offscreen();
    }

    pub(crate) fn render_argb8888(
        &mut self,
        #[cfg(feature = "text")] text_engine: &mut TextEngine,
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

        self.ctx
            .begin_frame(w, h, commands::to_femto_color(scene.background, 1.0));
        commands::encode_commands(
            &mut self.ctx.canvas,
            &mut self.textures,
            #[cfg(feature = "text")]
            text_engine,
            scene,
        );
        self.ctx.flush_offscreen_pixels(w, h, dst)
    }

    pub(crate) fn present_wayland_surface(
        &mut self,
        #[cfg(feature = "text")] text_engine: &mut TextEngine,
        conn: &Connection,
        surface: &WlSurface,
        key: RetainedSurfaceKey,
        logical_size: LogicalSize,
        scene: FrameScene<'_>,
    ) -> Result<bool> {
        let (buf_w, buf_h) = logical_size.to_buffer_ceil(scene.scale);
        let w = buf_w.as_u32();
        let h = buf_h.as_u32();
        if !self.ctx.ensure_wayland_target(conn, surface, key, w, h) {
            return Ok(false);
        }
        self.ctx
            .begin_frame(w, h, commands::to_femto_color(scene.background, 1.0));
        commands::encode_commands(
            &mut self.ctx.canvas,
            &mut self.textures,
            #[cfg(feature = "text")]
            text_engine,
            scene,
        );
        Ok(self.ctx.present_surface_frame(key))
    }

    pub(crate) fn drop_surface_target(&mut self, key: RetainedSurfaceKey) {
        self.ctx.drop_surface(key);
    }
}
