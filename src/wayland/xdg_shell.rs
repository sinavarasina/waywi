use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_positioner;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use smithay_client_toolkit::shell::xdg::popup::Popup;
use smithay_client_toolkit::shell::xdg::window::{DecorationMode, Window, WindowDecorations};
use smithay_client_toolkit::shell::xdg::{XdgPositioner, XdgShell, XdgSurface};
use wayland_client::protocol::{wl_seat, wl_surface};
use wayland_client::{Proxy, QueueHandle};

use crate::error::{Result, WaywiError};
use crate::reconcile::SurfaceSpec;
use crate::surface::{
    PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupPositioner, WindowDecoration,
};
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::SurfaceRole;

/// Creates an explicit fallback XDG toplevel window (`Surface::window`) when layer-shell is
/// unsupported or a standard desktop window is desired.
pub(crate) fn create_xdg_window(
    xdg_shell: &XdgShell,
    qh: &QueueHandle<WaylandState>,
    wl_surface: wl_surface::WlSurface,
    spec: &SurfaceSpec,
) -> Window {
    let deco = match spec.decoration {
        WindowDecoration::Auto => WindowDecorations::ServerDefault,
        WindowDecoration::Server => WindowDecorations::RequestServer,
        WindowDecoration::Client => WindowDecorations::RequestClient,
        WindowDecoration::None => WindowDecorations::None,
    };
    let window = xdg_shell.create_window(wl_surface, deco, qh);
    apply_xdg_spec(&window, spec, false);
    window.commit();
    window
}

/// Applies mutable XDG window title, app ID, size hints, and optional decoration changes from
/// [`SurfaceSpec`].
pub(crate) fn apply_xdg_spec(window: &Window, spec: &SurfaceSpec, decoration_changed: bool) {
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

    if decoration_changed {
        match spec.decoration {
            WindowDecoration::Auto => window.request_decoration_mode(None),
            WindowDecoration::Server => {
                window.request_decoration_mode(Some(DecorationMode::Server))
            }
            WindowDecoration::Client => {
                window.request_decoration_mode(Some(DecorationMode::Client))
            }
            WindowDecoration::None => {}
        }
    }
}

/// Builds a configured `xdg_positioner` from a WayWi [`PopupPositioner`] and logical popup size.
pub(crate) fn build_xdg_positioner(
    xdg_shell: &XdgShell,
    spec: &SurfaceSpec,
    pos_spec: &PopupPositioner,
) -> Result<XdgPositioner> {
    let positioner = XdgPositioner::new(xdg_shell)
        .map_err(|e| WaywiError::SurfaceError(format!("failed to create xdg_positioner: {e}")))?;

    let size = spec
        .popup
        .and_then(|p| p.desired_size)
        .unwrap_or(spec.logical_size);
    let w = size.width.0.round().max(1.0) as i32;
    let h = size.height.0.round().max(1.0) as i32;
    positioner.set_size(w, h);

    let ax = pos_spec.anchor_rect.x.0.round() as i32;
    let ay = pos_spec.anchor_rect.y.0.round() as i32;
    let aw = pos_spec.anchor_rect.width.0.round().max(1.0) as i32;
    let ah = pos_spec.anchor_rect.height.0.round().max(1.0) as i32;
    positioner.set_anchor_rect(ax, ay, aw, ah);

    positioner.set_anchor(map_popup_anchor(pos_spec.anchor));
    positioner.set_gravity(map_popup_gravity(pos_spec.gravity));

    let ox = pos_spec.offset_x.0.round() as i32;
    let oy = pos_spec.offset_y.0.round() as i32;
    if ox != 0 || oy != 0 {
        positioner.set_offset(ox, oy);
    }

    let adj = map_constraint_adjustment(pos_spec.constraint_adjustment);
    if !adj.is_empty() {
        positioner.set_constraint_adjustment(adj);
    }

    if pos_spec.reactive && xdg_shell.xdg_wm_base().version() >= 3 {
        positioner.set_reactive();
    }

    Ok(positioner)
}

/// Creates a compositor-positioned `xdg_popup` attached to a parent layer-shell surface
/// (`zwlr_layer_surface_v1.get_popup`), XDG toplevel window, or parent popup.
pub(crate) fn create_xdg_popup(
    xdg_shell: &XdgShell,
    qh: &QueueHandle<WaylandState>,
    wl_surface: wl_surface::WlSurface,
    spec: &SurfaceSpec,
    parent_role: &SurfaceRole,
    grab_seat_serial: Option<(&wl_seat::WlSeat, u32)>,
) -> Result<Popup> {
    let popup_spec = spec.popup.ok_or_else(|| {
        WaywiError::SurfaceError("SurfaceKind::Popup is missing popup positioner spec".to_owned())
    })?;
    let positioner = build_xdg_positioner(xdg_shell, spec, &popup_spec.positioner)?;

    let popup = match parent_role {
        SurfaceRole::Layer(layer) => {
            let p =
                Popup::from_surface(None, &positioner, qh, wl_surface, xdg_shell).map_err(|e| {
                    WaywiError::SurfaceError(format!("failed to create layer xdg_popup: {e}"))
                })?;
            attach_popup_to_layer(layer, &p);
            p
        }
        SurfaceRole::Xdg(window) => Popup::from_surface(
            Some(window.xdg_surface()),
            &positioner,
            qh,
            wl_surface,
            xdg_shell,
        )
        .map_err(|e| WaywiError::SurfaceError(format!("failed to create window xdg_popup: {e}")))?,
        SurfaceRole::Popup(parent_popup) => Popup::from_surface(
            Some(parent_popup.xdg_surface()),
            &positioner,
            qh,
            wl_surface,
            xdg_shell,
        )
        .map_err(|e| WaywiError::SurfaceError(format!("failed to create nested xdg_popup: {e}")))?,
        #[cfg(feature = "session-lock")]
        SurfaceRole::SessionLock(_) => {
            return Err(WaywiError::SurfaceError(
                "xdg_popup cannot be attached to an ext_session_lock_v1 surface".to_owned(),
            ));
        }
    };

    if popup_spec.grab
        && let Some((seat, serial)) = grab_seat_serial
    {
        popup.xdg_popup().grab(seat, serial);
    }

    popup.wl_surface().commit();
    Ok(popup)
}

/// Requests `xdg_popup.reposition` (`xdg_wm_base >= 3`) when a live popup's positioner or size
/// changes.
pub(crate) fn reposition_xdg_popup(
    xdg_shell: &XdgShell,
    popup: &Popup,
    spec: &SurfaceSpec,
    token: u32,
) -> Result<()> {
    if xdg_shell.xdg_wm_base().version() < 3 {
        return Ok(());
    }
    let Some(popup_spec) = spec.popup else {
        return Ok(());
    };
    let positioner = build_xdg_positioner(xdg_shell, spec, &popup_spec.positioner)?;
    popup.reposition(&positioner, token);
    Ok(())
}

fn attach_popup_to_layer(layer: &LayerSurface, popup: &Popup) {
    layer.get_popup(popup.xdg_popup());
}

fn map_popup_anchor(anchor: PopupAnchor) -> xdg_positioner::Anchor {
    match anchor {
        PopupAnchor::None => xdg_positioner::Anchor::None,
        PopupAnchor::Top => xdg_positioner::Anchor::Top,
        PopupAnchor::Bottom => xdg_positioner::Anchor::Bottom,
        PopupAnchor::Left => xdg_positioner::Anchor::Left,
        PopupAnchor::Right => xdg_positioner::Anchor::Right,
        PopupAnchor::TopLeft => xdg_positioner::Anchor::TopLeft,
        PopupAnchor::BottomLeft => xdg_positioner::Anchor::BottomLeft,
        PopupAnchor::TopRight => xdg_positioner::Anchor::TopRight,
        PopupAnchor::BottomRight => xdg_positioner::Anchor::BottomRight,
    }
}

fn map_popup_gravity(gravity: PopupGravity) -> xdg_positioner::Gravity {
    match gravity {
        PopupGravity::None => xdg_positioner::Gravity::None,
        PopupGravity::Top => xdg_positioner::Gravity::Top,
        PopupGravity::Bottom => xdg_positioner::Gravity::Bottom,
        PopupGravity::Left => xdg_positioner::Gravity::Left,
        PopupGravity::Right => xdg_positioner::Gravity::Right,
        PopupGravity::TopLeft => xdg_positioner::Gravity::TopLeft,
        PopupGravity::BottomLeft => xdg_positioner::Gravity::BottomLeft,
        PopupGravity::TopRight => xdg_positioner::Gravity::TopRight,
        PopupGravity::BottomRight => xdg_positioner::Gravity::BottomRight,
    }
}

fn map_constraint_adjustment(
    flags: PopupConstraintAdjustment,
) -> xdg_positioner::ConstraintAdjustment {
    let mut out = xdg_positioner::ConstraintAdjustment::empty();
    if flags.contains(PopupConstraintAdjustment::SLIDE_X) {
        out |= xdg_positioner::ConstraintAdjustment::SlideX;
    }
    if flags.contains(PopupConstraintAdjustment::SLIDE_Y) {
        out |= xdg_positioner::ConstraintAdjustment::SlideY;
    }
    if flags.contains(PopupConstraintAdjustment::FLIP_X) {
        out |= xdg_positioner::ConstraintAdjustment::FlipX;
    }
    if flags.contains(PopupConstraintAdjustment::FLIP_Y) {
        out |= xdg_positioner::ConstraintAdjustment::FlipY;
    }
    if flags.contains(PopupConstraintAdjustment::RESIZE_X) {
        out |= xdg_positioner::ConstraintAdjustment::ResizeX;
    }
    if flags.contains(PopupConstraintAdjustment::RESIZE_Y) {
        out |= xdg_positioner::ConstraintAdjustment::ResizeY;
    }
    out
}
