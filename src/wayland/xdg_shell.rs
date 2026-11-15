use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowDecorations};
use wayland_client::{QueueHandle, protocol::wl_surface};

use crate::reconcile::SurfaceSpec;
use crate::wayland::connection::WaylandState;

/// Creates an explicit fallback XDG toplevel window (`Surface::window`) when layer-shell is
/// unsupported or a standard desktop window is desired.
pub(crate) fn create_xdg_window(
    xdg_shell: &XdgShell,
    qh: &QueueHandle<WaylandState>,
    wl_surface: wl_surface::WlSurface,
    spec: &SurfaceSpec,
) -> Window {
    let window = xdg_shell.create_window(wl_surface, WindowDecorations::RequestServer, qh);
    apply_xdg_spec(&window, spec);
    window.commit();
    window
}

/// Applies mutable XDG window title, app ID, and size hints from [`SurfaceSpec`].
pub(crate) fn apply_xdg_spec(window: &Window, spec: &SurfaceSpec) {
    let title = if spec.title.is_empty() {
        if spec.namespace.is_empty() {
            "WayWi"
        } else {
            spec.namespace.as_str()
        }
    } else {
        spec.title.as_str()
    };
    window.set_title(title);

    let app_id = if spec.namespace.is_empty() {
        "org.waywi.app"
    } else {
        spec.namespace.as_str()
    };
    window.set_app_id(app_id);

    let w = spec.logical_size.width.0.max(1.0).round() as u32;
    let h = spec.logical_size.height.0.max(1.0).round() as u32;
    window.set_min_size(Some((w, h)));
}
