//! Visual primitives, layout containers, procedural [`Canvas`], memoization, and
//! CPU/GPU rendering backends.

pub(crate) mod backend;
pub mod canvas;
pub mod elements;
#[cfg(feature = "renderer-gpu")]
pub(crate) mod gpu;
pub(crate) mod memo;
pub(crate) mod retained;
pub(crate) mod software;
pub mod style;

pub use backend::RendererPreference;
pub use canvas::{Canvas, Painter, PathBuilder, PathOp};
pub use elements::{Clip, Column, Element, Group, Rect, Row};
pub use style::{Align, Border, Color, CornerRadius};

use wayland_client::Connection;
use wayland_client::protocol::wl_surface::WlSurface;

use crate::app::MemoryConfig;
use crate::asset::AssetId;
use crate::capabilities::Capability;
use crate::error::{Result, WaywiError};
#[cfg(not(feature = "text"))]
use crate::geometry::LogicalPx;
use crate::geometry::{BufferPx, BufferRect, LogicalSize, ScaleFactor};
use crate::input::NodeId;
use crate::reconcile::RetainedSurfaceKey;
#[cfg(feature = "text")]
use crate::text::TextEngine;
use crate::text::{SharedStr, TextMeasureStyle};

pub(crate) use backend::{FrameScene, RenderBackend};
pub(crate) use elements::ElementKind;
#[cfg(feature = "renderer-gpu")]
pub(crate) use gpu::GpuRenderer;
pub(crate) use memo::{MemoStore, begin_memo_check, enter_memo_scope, set_memo_surface_hash};
pub(crate) use retained::{
    CanvasDrawHandle, DrawCommand, RetainedNode, RetainedVisual, collect_draw_and_hit,
};
pub(crate) use software::SoftwareRenderer;

enum ActiveBackend {
    Software(Box<SoftwareRenderer>),
    #[cfg(feature = "renderer-gpu")]
    Gpu(Box<GpuRenderer>),
}

/// Unified runtime renderer owning one shared [`TextEngine`] and dispatching to either GPU or CPU.
pub(crate) struct ActiveRenderer {
    backend: ActiveBackend,
    #[cfg(feature = "text")]
    text_engine: TextEngine,
}

impl ActiveRenderer {
    pub(crate) fn from_preference(
        pref: RendererPreference,
        wayland_conn: Option<&Connection>,
        memory: &MemoryConfig,
    ) -> Result<Self> {
        let make_software = || {
            ActiveBackend::Software(Box::new(SoftwareRenderer::with_limits(
                memory.software_scratch_max_bytes,
                memory.canvas_cache_max_bytes,
            )))
        };

        let backend = match pref {
            RendererPreference::Software => {
                if !cfg!(feature = "renderer-software") {
                    return Err(WaywiError::UnsupportedCapability(
                        Capability::SoftwareRenderer,
                    ));
                }
                make_software()
            }
            #[cfg(feature = "renderer-gpu")]
            RendererPreference::Auto => {
                if let Some(gpu) = GpuRenderer::try_new(
                    wayland_conn,
                    memory.gpu_cache_max_bytes,
                    memory.gpu_stale_frame_age,
                ) {
                    ActiveBackend::Gpu(Box::new(gpu))
                } else {
                    make_software()
                }
            }
            #[cfg(not(feature = "renderer-gpu"))]
            RendererPreference::Auto => {
                let _ = wayland_conn;
                make_software()
            }
            #[cfg(feature = "renderer-gpu")]
            RendererPreference::Gpu => {
                let gpu = GpuRenderer::try_new(
                    wayland_conn,
                    memory.gpu_cache_max_bytes,
                    memory.gpu_stale_frame_age,
                )
                .ok_or(WaywiError::UnsupportedCapability(Capability::GpuRenderer))?;
                ActiveBackend::Gpu(Box::new(gpu))
            }
            #[cfg(not(feature = "renderer-gpu"))]
            RendererPreference::Gpu => {
                let _ = wayland_conn;
                return Err(WaywiError::UnsupportedCapability(Capability::GpuRenderer));
            }
        };

        Ok(Self {
            backend,
            #[cfg(feature = "text")]
            text_engine: TextEngine::with_config(
                memory.text_max_cached_buffers,
                memory.text_stale_frame_age,
                memory.load_system_fonts,
            ),
        })
    }

    pub(crate) fn is_gpu_active(&self) -> bool {
        #[cfg(feature = "renderer-gpu")]
        {
            matches!(self.backend, ActiveBackend::Gpu(_))
        }
        #[cfg(not(feature = "renderer-gpu"))]
        {
            false
        }
    }
}

impl RenderBackend for ActiveRenderer {
    fn drop_canvas_cache_for(&mut self, node_ids: &[NodeId]) {
        match &mut self.backend {
            ActiveBackend::Software(r) => r.drop_canvas_cache_for(node_ids),
            #[cfg(feature = "renderer-gpu")]
            ActiveBackend::Gpu(r) => r.drop_canvas_cache_for(node_ids),
        }
    }

    fn evict_asset(&mut self, asset_id: AssetId) {
        #[cfg(feature = "renderer-gpu")]
        if let ActiveBackend::Gpu(r) = &mut self.backend {
            r.evict_asset(asset_id);
        }
        #[cfg(not(feature = "renderer-gpu"))]
        let _ = asset_id;
    }

    fn trim_memory(&mut self) {
        match &mut self.backend {
            ActiveBackend::Software(r) => r.trim_memory(),
            #[cfg(feature = "renderer-gpu")]
            ActiveBackend::Gpu(r) => r.trim_memory(),
        }
        #[cfg(feature = "text")]
        self.text_engine.trim();
    }

    fn measure_text(
        &mut self,
        content: &SharedStr,
        style: TextMeasureStyle<'_>,
        scale: ScaleFactor,
        frame_seq: u64,
    ) -> LogicalSize {
        #[cfg(feature = "text")]
        {
            self.text_engine
                .measure_text(content, style, scale, frame_seq)
        }
        #[cfg(not(feature = "text"))]
        {
            let _ = (
                style.line_height_multiplier,
                style.max_width,
                style.font,
                style.align,
                style.bounds_mode,
                scale,
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
        dirty_rects: &[BufferRect],
        scene: backend::FrameScene<'_>,
    ) -> Result<()> {
        match &mut self.backend {
            ActiveBackend::Software(r) => r.render_argb8888(
                #[cfg(feature = "text")]
                &mut self.text_engine,
                dst_argb,
                buf_w,
                buf_h,
                dirty_rects,
                scene,
            ),
            #[cfg(feature = "renderer-gpu")]
            ActiveBackend::Gpu(r) => r.render_argb8888(
                #[cfg(feature = "text")]
                &mut self.text_engine,
                dst_argb,
                buf_w,
                buf_h,
                scene,
            ),
        }
    }

    #[cfg_attr(not(feature = "renderer-gpu"), allow(unused_variables))]
    fn present_wayland_surface(
        &mut self,
        conn: &Connection,
        surface: &WlSurface,
        key: RetainedSurfaceKey,
        logical_size: LogicalSize,
        scene: backend::FrameScene<'_>,
    ) -> Result<bool> {
        match &mut self.backend {
            ActiveBackend::Software(_) => Ok(false),
            #[cfg(feature = "renderer-gpu")]
            ActiveBackend::Gpu(r) => r.present_wayland_surface(
                #[cfg(feature = "text")]
                &mut self.text_engine,
                conn,
                surface,
                key,
                logical_size,
                scene,
            ),
        }
    }

    fn drop_surface_target(&mut self, key: RetainedSurfaceKey) {
        #[cfg(feature = "renderer-gpu")]
        if let ActiveBackend::Gpu(r) = &mut self.backend {
            r.drop_surface_target(key);
        }
        #[cfg(not(feature = "renderer-gpu"))]
        let _ = key;
    }
}
