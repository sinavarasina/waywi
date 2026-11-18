use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use smithay_client_toolkit::shell::xdg::window::Window;
use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_surface;

#[cfg(feature = "fractional-scale")]
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1;
#[cfg(feature = "fractional-scale")]
use wayland_protocols::wp::viewporter::client::wp_viewport;

use crate::geometry::{LogicalSize, ScaleFactor};
use crate::input::{Key, Modifiers, MouseButton};
use crate::reconcile::{RetainedSurfaceKey, SurfaceSpec};
use crate::wayland::shm_pool::SurfaceShmPool;

/// Wayland compositor role backing a retained surface.
pub(crate) enum SurfaceRole {
    Layer(LayerSurface),
    Xdg(Window),
}

impl SurfaceRole {
    pub(crate) fn wl_surface(&self) -> &wl_surface::WlSurface {
        match self {
            Self::Layer(l) => l.wl_surface(),
            Self::Xdg(w) => w.wl_surface(),
        }
    }
}

/// Live Wayland resources associated with a single [`RetainedSurfaceKey`].
pub(crate) struct LiveWaylandSurface {
    pub(crate) role: SurfaceRole,
    pub(crate) wl_surface_id: ObjectId,
    #[cfg(feature = "fractional-scale")]
    pub(crate) fractional_scale: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    #[cfg(feature = "fractional-scale")]
    pub(crate) viewport: Option<wp_viewport::WpViewport>,
    pub(crate) pool: SurfaceShmPool,
    pub(crate) configured: bool,
    pub(crate) configured_size: Option<LogicalSize>,
    pub(crate) scale: ScaleFactor,
    pub(crate) frame_pending: bool,
    pub(crate) pending_redraw: bool,
    pub(crate) spec: SurfaceSpec,
}

impl Drop for LiveWaylandSurface {
    fn drop(&mut self) {
        self.pool.shrink_and_unmap();
        #[cfg(feature = "fractional-scale")]
        if let Some(fs) = self.fractional_scale.take() {
            fs.destroy();
        }
        #[cfg(feature = "fractional-scale")]
        if let Some(vp) = self.viewport.take() {
            vp.destroy();
        }
    }
}

/// Discrete events produced by the Wayland queue for [`crate::App`].
#[derive(Debug, Clone)]
pub(crate) enum WaylandEvent {
    OutputsChanged,
    SurfaceConfigured {
        key: RetainedSurfaceKey,
        size: LogicalSize,
    },
    SurfaceScaleChanged {
        key: RetainedSurfaceKey,
        scale: ScaleFactor,
    },
    SurfaceClosed {
        key: RetainedSurfaceKey,
    },
    SurfaceFrame {
        key: RetainedSurfaceKey,
    },
    BufferReleased {
        key: RetainedSurfaceKey,
    },
    PointerEnter {
        key: RetainedSurfaceKey,
        x: f32,
        y: f32,
        modifiers: Modifiers,
    },
    PointerLeave {
        key: RetainedSurfaceKey,
    },
    PointerMotion {
        key: RetainedSurfaceKey,
        x: f32,
        y: f32,
        modifiers: Modifiers,
    },
    PointerButton {
        key: RetainedSurfaceKey,
        x: f32,
        y: f32,
        button: MouseButton,
        pressed: bool,
        modifiers: Modifiers,
    },
    PointerScroll {
        key: RetainedSurfaceKey,
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
        discrete_x: i32,
        discrete_y: i32,
        modifiers: Modifiers,
    },
    TouchDown {
        key: RetainedSurfaceKey,
        id: i32,
        x: f32,
        y: f32,
    },
    TouchMotion {
        id: i32,
        x: f32,
        y: f32,
    },
    TouchUp {
        id: i32,
    },
    KeyboardKey {
        key: RetainedSurfaceKey,
        pressed: bool,
        repeat: bool,
        logical_key: Key,
        modifiers: Modifiers,
        text: Option<String>,
    },
    KeyboardLayoutChanged(crate::input::KeyboardLayoutInfo),
    #[cfg(feature = "idle-notify")]
    Idle {
        timeout: std::time::Duration,
        input_only: bool,
        idle: bool,
    },
    #[cfg(feature = "clipboard")]
    Clipboard(crate::clipboard::ClipboardEvent),
    #[cfg(feature = "ime")]
    Ime {
        key: RetainedSurfaceKey,
        event: crate::input::ImeEvent,
    },
}
