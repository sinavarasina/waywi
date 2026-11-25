//! Wayland surface lifecycle, outputs, layer-shell, xdg-window, SHM, and registry delegates.

use smithay_client_toolkit::activation::ActivationHandler;
use smithay_client_toolkit::compositor::CompositorHandler;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::xdg::popup::{Popup, PopupConfigure, PopupHandler};
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowHandler};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use wayland_client::protocol::{wl_output, wl_surface};
use wayland_client::{Connection, Proxy, QueueHandle};

use crate::geometry::{LogicalPx, LogicalSize, ScaleFactor};
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::{ActivationRequestData, WaylandEvent};

fn handle_surface_configure(
    state: &mut WaylandState,
    wl_surface: &wl_surface::WlSurface,
    width_hint: Option<u32>,
    height_hint: Option<u32>,
) {
    if let Some(&key) = state.surface_by_obj.get(&wl_surface.id())
        && let Some(surf) = state.surfaces.get_mut(&key)
    {
        let w = width_hint
            .filter(|&v| v > 0)
            .map_or(surf.spec.logical_size.width.0.max(1.0), |v| v as f32);
        let h = height_hint
            .filter(|&v| v > 0)
            .map_or(surf.spec.logical_size.height.0.max(1.0), |v| v as f32);
        let size = LogicalSize::new(LogicalPx(w), LogicalPx(h));
        let was_configured = surf.configured;
        let size_changed = surf.configured_size != Some(size);
        surf.configured = true;
        surf.configured_size = Some(size);
        // Any compositor `configure` (including same-size state/HDR/VRR/fullscreen transitions)
        // expects a fresh commit and clears any stale `frame_pending` gate.
        surf.frame_pending = false;
        if !was_configured || size_changed {
            surf.pending_redraw = true;
        }
        state
            .events
            .push_back(WaylandEvent::SurfaceConfigured { key, size });
    }
}

impl CompositorHandler for WaylandState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if let Some(&key) = self.surface_by_obj.get(&surface.id())
            && let Some(surf) = self.surfaces.get_mut(&key)
        {
            #[cfg(feature = "fractional-scale")]
            if surf.fractional_scale.is_some() {
                return;
            }
            let scale = ScaleFactor::from_integer(new_factor.max(1));
            if surf.scale != scale {
                surf.scale = scale;
                surf.frame_pending = false;
                surf.pending_redraw = true;
                self.events
                    .push_back(WaylandEvent::SurfaceScaleChanged { key, scale });
            }
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if let Some(&key) = self.surface_by_obj.get(&surface.id())
            && let Some(surf) = self.surfaces.get_mut(&key)
        {
            surf.frame_pending = false;
            self.events.push_back(WaylandEvent::SurfaceFrame { key });
        }
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for WaylandState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        self.refresh_outputs();
        self.events.push_back(WaylandEvent::OutputsChanged);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        self.refresh_outputs();
        self.events.push_back(WaylandEvent::OutputsChanged);
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        // NOTE: SCTK invokes `output_destroyed` BEFORE removing the output from
        // `self.output_state.outputs`, so `WaylandEvent::OutputsChanged` refreshes
        // `wl.outputs` during event-loop dispatch once SCTK has finished mutating `output_state`.
        self.events.push_back(WaylandEvent::OutputsChanged);
    }
}

impl LayerShellHandler for WaylandState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(&key) = self.surface_by_obj.get(&layer.wl_surface().id()) {
            self.events.push_back(WaylandEvent::LayerClosed { key });
        }
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        handle_surface_configure(
            self,
            layer.wl_surface(),
            Some(configure.new_size.0),
            Some(configure.new_size.1),
        );
    }
}

impl WindowHandler for WaylandState {
    fn request_close(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, window: &Window) {
        if let Some(&key) = self.surface_by_obj.get(&window.wl_surface().id()) {
            self.events
                .push_back(WaylandEvent::WindowCloseRequested { key });
        }
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        handle_surface_configure(
            self,
            window.wl_surface(),
            configure.new_size.0.map(u32::from),
            configure.new_size.1.map(u32::from),
        );
    }
}

impl PopupHandler for WaylandState {
    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        popup: &Popup,
        config: PopupConfigure,
    ) {
        handle_surface_configure(
            self,
            popup.wl_surface(),
            u32::try_from(config.width).ok().filter(|&v| v > 0),
            u32::try_from(config.height).ok().filter(|&v| v > 0),
        );
    }

    fn done(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, popup: &Popup) {
        if let Some(&key) = self.surface_by_obj.get(&popup.wl_surface().id()) {
            self.events.push_back(WaylandEvent::PopupDismissed { key });
        }
    }
}

impl ActivationHandler for WaylandState {
    type RequestData = ActivationRequestData;

    fn new_token(&mut self, token: String, data: &Self::RequestData) {
        self.events.push_back(WaylandEvent::ActivationToken {
            request_id: data.request_id,
            token,
        });
    }
}

impl ShmHandler for WaylandState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for WaylandState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
