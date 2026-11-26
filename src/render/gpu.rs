//! Hardware-accelerated GPU renderer powered by `femtovg` + `wgpu`.

pub(crate) mod commands;
pub(crate) mod context;
pub(crate) mod textures;

use wayland_client::Connection;

use crate::app::GpuMemoryPolicy;
use crate::asset::AssetId;
use crate::error::{Result, WaywiError};
use crate::geometry::BufferPx;
use crate::input::NodeId;
use crate::reconcile::RetainedSurfaceKey;
use crate::render::backend::{FrameScene, WaylandPresentOutcome, WaylandPresentTarget};
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
        memory_policy: GpuMemoryPolicy,
    ) -> Option<Self> {
        let ctx = GpuContext::new(wayland_conn, memory_policy)?;
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
        target: WaylandPresentTarget<'_>,
        scene: FrameScene<'_>,
        on_before_commit: &mut dyn FnMut(),
    ) -> Result<WaylandPresentOutcome> {
        let (buf_w, buf_h) = target.logical_size.to_buffer_ceil(scene.scale);
        let w = buf_w.as_u32();
        let h = buf_h.as_u32();
        if w == 0 || h == 0 {
            return Ok(if self.ctx.has_wayland_target(target.key) {
                WaylandPresentOutcome::Deferred
            } else {
                WaylandPresentOutcome::FallbackToShm
            });
        }
        if !self
            .ctx
            .ensure_wayland_target(target.conn, target.surface, target.key, w, h)
        {
            return Ok(WaylandPresentOutcome::FallbackToShm);
        }
        let Some(frame) =
            self.ctx
                .acquire_surface_frame(target.conn, target.surface, target.key, w, h)
        else {
            // Once bound to a Vulkan/EGL swapchain, never fall back to `wl_shm` on the same
            // `wl_surface`; defer until the compositor resumes presenting (e.g. DPMS wake).
            return Ok(WaylandPresentOutcome::Deferred);
        };
        self.ctx
            .begin_frame(w, h, commands::to_femto_color(scene.background, 1.0));
        commands::encode_commands(
            &mut self.ctx.canvas,
            &mut self.textures,
            #[cfg(feature = "text")]
            text_engine,
            scene,
        );
        on_before_commit();
        self.ctx.present_acquired_frame(frame);
        Ok(WaylandPresentOutcome::Committed)
    }

    pub(crate) fn drop_surface_target(&mut self, key: RetainedSurfaceKey) {
        self.ctx.drop_surface(key);
    }
}
