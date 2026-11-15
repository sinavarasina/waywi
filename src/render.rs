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

use crate::error::Result;
use crate::geometry::{BufferPx, BufferRect, LogicalSize};
use crate::input::NodeId;
use crate::reconcile::RetainedSurfaceKey;
use crate::text::{SharedStr, TextMeasureStyle};

pub(crate) use backend::{FrameScene, RenderBackend};
pub(crate) use elements::ElementKind;
#[cfg(feature = "renderer-gpu")]
pub(crate) use gpu::GpuRenderer;
pub(crate) use memo::{MemoStore, check_memo, enter_memo_scope, set_memo_surface_hash};
pub(crate) use retained::{
    CanvasDrawHandle, DrawCommand, RetainedNode, RetainedVisual, collect_draw_and_hit,
};
pub(crate) use software::SoftwareRenderer;

/// Runtime wrapper dispatching to the chosen rendering backend ([`GpuRenderer`] or [`SoftwareRenderer`]).
pub(crate) enum ActiveRenderer {
    Software(Box<SoftwareRenderer>),
    #[cfg(feature = "renderer-gpu")]
    Gpu(Box<GpuRenderer>),
}

impl ActiveRenderer {
    pub(crate) fn from_preference(
        pref: RendererPreference,
        wayland_conn: Option<&Connection>,
    ) -> Self {
        match pref {
            RendererPreference::Software => Self::Software(Box::default()),
            #[cfg(feature = "renderer-gpu")]
            RendererPreference::Auto => {
                if let Some(gpu) = GpuRenderer::try_new(wayland_conn) {
                    Self::Gpu(Box::new(gpu))
                } else {
                    Self::Software(Box::default())
                }
            }
            #[cfg(not(feature = "renderer-gpu"))]
            RendererPreference::Auto => {
                let _ = wayland_conn;
                Self::Software(Box::default())
            }
            #[cfg(feature = "renderer-gpu")]
            RendererPreference::Gpu => {
                Self::Gpu(Box::new(GpuRenderer::new_with_probe(wayland_conn)))
            }
            #[cfg(not(feature = "renderer-gpu"))]
            RendererPreference::Gpu => {
                let _ = wayland_conn;
                Self::Software(Box::default())
            }
        }
    }

    pub(crate) fn is_gpu_active(&self) -> bool {
        #[cfg(feature = "renderer-gpu")]
        {
            matches!(self, Self::Gpu(g) if g.is_available())
        }
        #[cfg(not(feature = "renderer-gpu"))]
        {
            false
        }
    }
}

impl RenderBackend for ActiveRenderer {
    fn drop_canvas_cache_for(&mut self, node_ids: &[NodeId]) {
        match self {
            Self::Software(r) => r.drop_canvas_cache_for(node_ids),
            #[cfg(feature = "renderer-gpu")]
            Self::Gpu(r) => r.drop_canvas_cache_for(node_ids),
        }
    }

    fn measure_text(
        &mut self,
        content: &SharedStr,
        style: TextMeasureStyle<'_>,
        frame_seq: u64,
    ) -> LogicalSize {
        match self {
            Self::Software(r) => r.measure_text(content, style, frame_seq),
            #[cfg(feature = "renderer-gpu")]
            Self::Gpu(r) => r.measure_text(content, style, frame_seq),
        }
    }

    fn render_argb8888(
        &mut self,
        dst_argb: &mut [u8],
        buf_w: BufferPx,
        buf_h: BufferPx,
        dirty_rects: &[BufferRect],
        scene: backend::FrameScene<'_>,
    ) -> Result<()> {
        match self {
            Self::Software(r) => r.render_argb8888(dst_argb, buf_w, buf_h, dirty_rects, scene),
            #[cfg(feature = "renderer-gpu")]
            Self::Gpu(r) => r.render_argb8888(dst_argb, buf_w, buf_h, dirty_rects, scene),
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
        match self {
            Self::Software(_) => Ok(false),
            #[cfg(feature = "renderer-gpu")]
            Self::Gpu(r) => r.present_wayland_surface(conn, surface, key, logical_size, scene),
        }
    }

    fn drop_surface_target(&mut self, key: RetainedSurfaceKey) {
        #[cfg(feature = "renderer-gpu")]
        if let Self::Gpu(r) = self {
            r.drop_surface_target(key);
        }
        #[cfg(not(feature = "renderer-gpu"))]
        let _ = key;
    }
}
