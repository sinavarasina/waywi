//! Abstract rendering backend contract implemented by [`super::SoftwareRenderer`] and
//! [`super::GpuRenderer`](crate::render::GpuRenderer).

use wayland_client::Connection;
use wayland_client::protocol::wl_surface::WlSurface;

use crate::asset::{AssetCache, AssetId};
use crate::error::Result;
use crate::geometry::{BufferPx, BufferRect, LogicalSize, ScaleFactor};
use crate::input::NodeId;
use crate::reconcile::RetainedSurfaceKey;
use crate::render::retained::DrawCommand;
use crate::render::style::Color;
use crate::text::{SharedStr, TextMeasureStyle};

/// Preferred rendering pipeline selected at application startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RendererPreference {
    /// Prefer hardware GPU rendering when enabled and a compatible adapter is available, falling
    /// back seamlessly to CPU `wl_shm` software rendering.
    #[default]
    Auto,
    /// Force CPU `wl_shm` software rendering.
    Software,
    /// Require GPU hardware rendering (`renderer-gpu` feature + available adapter).
    Gpu,
}

/// Per-frame scene parameters passed into [`RenderBackend`] methods.
#[derive(Clone, Copy)]
pub(crate) struct FrameScene<'a> {
    pub(crate) scale: ScaleFactor,
    pub(crate) background: Color,
    pub(crate) commands: &'a [DrawCommand],
    pub(crate) assets: &'a AssetCache,
    pub(crate) frame_seq: u64,
}

/// Unified renderer interface consumed by the reconciler and Wayland/headless presentation loops.
pub(crate) trait RenderBackend {
    /// Drops cached intermediate pixmaps/textures owned by retired canvas/text nodes.
    fn drop_canvas_cache_for(&mut self, node_ids: &[NodeId]);

    /// Drops any backend textures cached for `asset_id` (e.g. after pixel eviction/unload).
    fn evict_asset(&mut self, _asset_id: AssetId) {}

    /// Trims transient scratch buffers and stale renderer caches.
    fn trim_memory(&mut self) {}

    /// Measures wrapped/aligned text extents in logical coordinates for layout passes.
    fn measure_text(
        &mut self,
        content: &SharedStr,
        style: TextMeasureStyle<'_>,
        scale: ScaleFactor,
        frame_seq: u64,
    ) -> LogicalSize;

    /// Measures shaped text bounds together with exact proportional cursor coordinates.
    #[cfg(feature = "text")]
    fn measure_text_cursor(
        &mut self,
        content: &SharedStr,
        style: TextMeasureStyle<'_>,
        cursor_byte: usize,
        frame_seq: u64,
    ) -> crate::text::TextCursorMetrics;

    /// Renders `scene` into an `ARGB8888` pre-multiplied pixel buffer across `dirty_rects`.
    fn render_argb8888(
        &mut self,
        dst_argb: &mut [u8],
        buf_w: BufferPx,
        buf_h: BufferPx,
        dirty_rects: &[BufferRect],
        scene: FrameScene<'_>,
    ) -> Result<()>;

    /// Presents directly to a live `wl_surface` when supported by a GPU swapchain backend.
    ///
    /// Returns `Ok(true)` if the frame was committed via hardware swapchain, or `Ok(false)` to
    /// fall back cleanly to `wl_shm` presentation.
    fn present_wayland_surface(
        &mut self,
        _conn: &Connection,
        _surface: &WlSurface,
        _key: RetainedSurfaceKey,
        _logical_size: LogicalSize,
        _scene: FrameScene<'_>,
    ) -> Result<bool> {
        Ok(false)
    }

    /// Releases any GPU swapchain resources associated with a destroyed Wayland surface.
    fn drop_surface_target(&mut self, _key: RetainedSurfaceKey) {}
}
