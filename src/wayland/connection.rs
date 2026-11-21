use std::collections::{HashMap, VecDeque};

use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::globals::ProvidesBoundGlobal;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shm::Shm;
use wayland_client::backend::ObjectId;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_touch};
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};

#[cfg(feature = "fractional-scale")]
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1;
#[cfg(feature = "fractional-scale")]
use wayland_protocols::wp::viewporter::client::wp_viewporter;

use crate::capabilities::{Capabilities, Capability};
use crate::display::OutputInfo;
use crate::error::{Result, WaywiError};
use crate::geometry::{BufferRect, GeoRect, LogicalSize, ScaleFactor};
use crate::input::Modifiers;
use crate::reconcile::{RetainedSurfaceKey, SurfaceSpec};
use crate::surface::{InputMode, SurfaceKind};
use crate::wayland::layer_shell::{apply_layer_spec, create_layer_surface};
use crate::wayland::output::collect_outputs;
use crate::wayland::shm_pool::{AcquireSlot, SurfaceShmPool};
use crate::wayland::surface_state::{LiveWaylandSurface, SurfaceRole, WaylandEvent};
use crate::wayland::xdg_shell::{apply_xdg_spec, create_xdg_window};

/// Complete SCTK + Wayland protocol state dispatched from `calloop`.
pub(crate) struct WaylandState {
    pub(crate) registry_state: RegistryState,
    pub(crate) compositor: CompositorState,
    pub(crate) output_state: OutputState,
    pub(crate) seat_state: SeatState,
    pub(crate) shm: Shm,
    pub(crate) layer_shell: Option<LayerShell>,
    pub(crate) xdg_shell: Option<XdgShell>,
    #[cfg(feature = "fractional-scale")]
    pub(crate) viewporter: Option<wp_viewporter::WpViewporter>,
    #[cfg(feature = "fractional-scale")]
    pub(crate) fractional_scale_mgr:
        Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    #[cfg(feature = "idle-notify")]
    pub(crate) idle: crate::wayland::idle::WaylandIdleState,
    #[cfg(feature = "clipboard")]
    pub(crate) clipboard: crate::wayland::clipboard::WaylandClipboardState,
    #[cfg(feature = "ime")]
    pub(crate) ime: crate::wayland::ime::WaylandImeState,

    pub(crate) outputs: Vec<OutputInfo>,
    pub(crate) output_handles: HashMap<crate::display::OutputId, wl_output::WlOutput>,
    pub(crate) surfaces: HashMap<RetainedSurfaceKey, LiveWaylandSurface>,
    pub(crate) surface_by_obj: HashMap<ObjectId, RetainedSurfaceKey>,

    pub(crate) keyboards: Vec<wl_keyboard::WlKeyboard>,
    pub(crate) pointers: Vec<wl_pointer::WlPointer>,
    pub(crate) touches: Vec<wl_touch::WlTouch>,
    pub(crate) has_keyboard: bool,
    pub(crate) has_pointer: bool,
    pub(crate) has_touch: bool,
    pub(crate) focused_keyboard_surface: Option<RetainedSurfaceKey>,
    pub(crate) active_touches: HashMap<i32, RetainedSurfaceKey>,
    pub(crate) modifiers: Modifiers,
    pub(crate) keyboard_layouts: Vec<String>,
    pub(crate) keyboard_layout_index: u32,

    pub(crate) events: VecDeque<WaylandEvent>,
}

impl WaylandState {
    pub(crate) fn init(conn: &Connection) -> Result<(Self, EventQueue<Self>, QueueHandle<Self>)> {
        let (globals, mut queue) = registry_queue_init::<Self>(conn).map_err(|e| {
            WaywiError::WaylandConnection(format!("registry_queue_init failed: {e}"))
        })?;
        let qh = queue.handle();

        let compositor = CompositorState::bind(&globals, &qh).map_err(|e| {
            WaywiError::WaylandConnection(format!("wl_compositor unavailable: {e}"))
        })?;
        let shm = Shm::bind(&globals, &qh)
            .map_err(|e| WaywiError::WaylandConnection(format!("wl_shm unavailable: {e}")))?;
        let layer_shell = LayerShell::bind(&globals, &qh).ok();
        let xdg_shell = XdgShell::bind(&globals, &qh).ok();

        #[cfg(feature = "fractional-scale")]
        let viewporter = globals
            .bind::<wp_viewporter::WpViewporter, _, _>(&qh, 1..=1, ())
            .ok();
        #[cfg(feature = "fractional-scale")]
        let fractional_scale_mgr = globals
            .bind::<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, _, _>(
                &qh,
                1..=1,
                (),
            )
            .ok();
        #[cfg(feature = "idle-notify")]
        let idle = crate::wayland::idle::WaylandIdleState::bind(&globals, &qh);
        #[cfg(feature = "clipboard")]
        let clipboard = crate::wayland::clipboard::WaylandClipboardState::bind(&globals, &qh);
        #[cfg(feature = "ime")]
        let ime = crate::wayland::ime::WaylandImeState::bind(&globals, &qh);

        let output_state = OutputState::new(&globals, &qh);
        let seat_state = SeatState::new(&globals, &qh);
        let registry_state = RegistryState::new(&globals);

        let mut state = Self {
            registry_state,
            compositor,
            output_state,
            seat_state,
            shm,
            layer_shell,
            xdg_shell,
            #[cfg(feature = "fractional-scale")]
            viewporter,
            #[cfg(feature = "fractional-scale")]
            fractional_scale_mgr,
            #[cfg(feature = "idle-notify")]
            idle,
            #[cfg(feature = "clipboard")]
            clipboard,
            #[cfg(feature = "ime")]
            ime,
            outputs: Vec::new(),
            output_handles: HashMap::new(),
            surfaces: HashMap::new(),
            surface_by_obj: HashMap::new(),
            keyboards: Vec::new(),
            pointers: Vec::new(),
            touches: Vec::new(),
            has_keyboard: false,
            has_pointer: false,
            has_touch: false,
            focused_keyboard_surface: None,
            active_touches: HashMap::new(),
            modifiers: Modifiers::empty(),
            keyboard_layouts: Vec::new(),
            keyboard_layout_index: 0,
            events: VecDeque::new(),
        };

        // Two initial roundtrips populate advertised outputs, seats, and initial formats.
        queue.roundtrip(&mut state).map_err(|e| {
            WaywiError::WaylandConnection(format!("initial wayland roundtrip failed: {e}"))
        })?;
        queue.roundtrip(&mut state).map_err(|e| {
            WaywiError::WaylandConnection(format!("second wayland roundtrip failed: {e}"))
        })?;
        #[cfg(feature = "ime")]
        {
            let seats: Vec<_> = state.seat_state.seats().collect();
            for seat in seats {
                state.ime.add_seat(&seat, &qh);
            }
        }
        state.refresh_outputs();

        Ok((state, queue, qh))
    }

    pub(crate) fn refresh_outputs(&mut self) {
        let (infos, map) = collect_outputs(&self.output_state);
        self.outputs = infos;
        self.output_handles = map;
    }

    pub(crate) fn keyboard_layout_info(&self) -> crate::input::KeyboardLayoutInfo {
        crate::input::KeyboardLayoutInfo::from_layouts(
            self.keyboard_layouts.clone(),
            self.keyboard_layout_index,
        )
    }

    #[cfg(feature = "idle-notify")]
    pub(crate) fn sync_idle_watches(
        &mut self,
        desired: &[(std::time::Duration, bool)],
        qh: &QueueHandle<Self>,
    ) {
        let seat = self.seat_state.seats().next();
        self.idle.sync(seat.as_ref(), desired, qh);
    }

    #[cfg(feature = "clipboard")]
    pub(crate) fn set_clipboard_watching(&mut self, watching: bool, qh: &QueueHandle<Self>) {
        let seat = self.seat_state.seats().next();
        self.clipboard.set_watching(watching, seat.as_ref(), qh);
    }

    #[cfg(feature = "clipboard")]
    pub(crate) fn set_clipboard_selection(
        &mut self,
        selection: crate::clipboard::ClipboardSelection,
        entries: Vec<(String, Vec<u8>)>,
        qh: &QueueHandle<Self>,
    ) -> Result<()> {
        let seat = self.seat_state.seats().next();
        self.clipboard
            .set_selection(seat.as_ref(), selection, entries, qh)
    }

    pub(crate) fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::from_compile_features();
        caps.set(Capability::Wayland, true);
        caps.set(Capability::LayerShell, self.layer_shell.is_some());
        caps.set(Capability::XdgShell, self.xdg_shell.is_some());
        #[cfg(feature = "fractional-scale")]
        {
            caps.set(Capability::Viewporter, self.viewporter.is_some());
            caps.set(
                Capability::FractionalScale,
                self.fractional_scale_mgr.is_some() && self.viewporter.is_some(),
            );
        }
        caps.set(Capability::PointerInput, self.has_pointer);
        caps.set(Capability::KeyboardInput, self.has_keyboard);
        caps.set(Capability::KeyboardLayout, self.has_keyboard);
        caps.set(Capability::TouchInput, self.has_touch);
        #[cfg(feature = "idle-notify")]
        caps.set(Capability::IdleNotify, self.idle.is_supported());
        #[cfg(not(feature = "idle-notify"))]
        caps.set(Capability::IdleNotify, false);
        #[cfg(feature = "clipboard")]
        caps.set(Capability::Clipboard, self.clipboard.is_supported());
        #[cfg(not(feature = "clipboard"))]
        caps.set(Capability::Clipboard, false);
        #[cfg(feature = "ime")]
        caps.set(Capability::TextInput, self.ime.is_supported());
        #[cfg(not(feature = "ime"))]
        caps.set(Capability::TextInput, false);
        #[cfg(feature = "pipewire")]
        caps.set(
            Capability::PipeWire,
            crate::audio::backend::pipewire_runtime_available(),
        );
        #[cfg(not(feature = "pipewire"))]
        caps.set(Capability::PipeWire, false);
        #[cfg(feature = "pulseaudio")]
        caps.set(
            Capability::PulseAudio,
            crate::audio::backend::pulseaudio_runtime_available(),
        );
        #[cfg(not(feature = "pulseaudio"))]
        caps.set(Capability::PulseAudio, false);
        #[cfg(feature = "alsa")]
        caps.set(
            Capability::Alsa,
            crate::audio::backend::alsa_runtime_available(),
        );
        #[cfg(not(feature = "alsa"))]
        caps.set(Capability::Alsa, false);
        caps.set(
            Capability::AudioPlayback,
            cfg!(feature = "audio-playback")
                || caps.has(Capability::PipeWire)
                || caps.has(Capability::PulseAudio)
                || caps.has(Capability::Alsa),
        );
        caps.set(
            Capability::HyprlandIpc,
            cfg!(feature = "hyprland-ipc")
                && std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some(),
        );
        caps.set(
            Capability::SwayIpc,
            cfg!(feature = "sway-ipc") && std::env::var_os("SWAYSOCK").is_some(),
        );
        caps.set(
            Capability::Dbus,
            cfg!(feature = "dbus") && std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(),
        );
        caps
    }

    pub(crate) fn create_surface(
        &mut self,
        qh: &QueueHandle<Self>,
        key: RetainedSurfaceKey,
        spec: &SurfaceSpec,
    ) -> Result<()> {
        if self.surfaces.contains_key(&key) {
            self.update_surface_spec(key, spec)?;
            return Ok(());
        }

        let wl_surface = self.compositor.create_surface(qh);
        let wl_surface_id = wl_surface.id();

        #[cfg(feature = "fractional-scale")]
        let viewport = self
            .viewporter
            .as_ref()
            .map(|vp| vp.get_viewport(&wl_surface, qh, ()));
        #[cfg(feature = "fractional-scale")]
        let fractional_scale = self
            .fractional_scale_mgr
            .as_ref()
            .map(|mgr| mgr.get_fractional_scale(&wl_surface, qh, key));

        let output_obj = spec
            .output_id
            .and_then(|id| self.output_handles.get(&id))
            .or_else(|| {
                self.outputs
                    .first()
                    .and_then(|o| self.output_handles.get(&o.id))
            });

        let initial_scale = spec
            .output_id
            .and_then(|id| self.outputs.iter().find(|o| o.id == id))
            .or_else(|| self.outputs.first())
            .map(|o| o.scale_factor)
            .unwrap_or(ScaleFactor::ONE);

        let role = match spec.kind {
            SurfaceKind::Window => {
                let xdg = self
                    .xdg_shell
                    .as_ref()
                    .ok_or(WaywiError::UnsupportedCapability(Capability::XdgShell))?;
                SurfaceRole::Xdg(create_xdg_window(xdg, qh, wl_surface, spec))
            }
            SurfaceKind::Panel | SurfaceKind::Floating | SurfaceKind::Overlay => {
                let layer_shell = self
                    .layer_shell
                    .as_ref()
                    .ok_or(WaywiError::UnsupportedCapability(Capability::LayerShell))?;
                SurfaceRole::Layer(create_layer_surface(
                    layer_shell,
                    qh,
                    wl_surface,
                    spec,
                    output_obj,
                ))
            }
        };

        self.surface_by_obj.insert(wl_surface_id.clone(), key);
        self.surfaces.insert(
            key,
            LiveWaylandSurface {
                role,
                wl_surface_id,
                #[cfg(feature = "fractional-scale")]
                fractional_scale,
                #[cfg(feature = "fractional-scale")]
                viewport,
                pool: SurfaceShmPool::default(),
                configured: false,
                configured_size: None,
                scale: initial_scale,
                frame_pending: false,
                pending_redraw: true,
                spec: spec.clone(),
            },
        );
        Ok(())
    }

    #[cfg(feature = "ime")]
    pub(crate) fn sync_ime_for_surface(
        &mut self,
        key: RetainedSurfaceKey,
        cfg: &crate::input::ImeConfig,
    ) {
        let Some(wl_id) = self.surfaces.get(&key).map(|s| s.wl_surface_id.clone()) else {
            return;
        };
        if self.ime.sync_surface(&wl_id, cfg) {
            self.commit_if_configured(key);
        }
    }

    pub(crate) fn update_surface_spec(
        &mut self,
        key: RetainedSurfaceKey,
        spec: &SurfaceSpec,
    ) -> Result<()> {
        let Some(surf) = self.surfaces.get_mut(&key) else {
            return Ok(());
        };
        if surf.spec != *spec {
            match &surf.role {
                SurfaceRole::Layer(layer) => apply_layer_spec(layer, spec),
                SurfaceRole::Xdg(window) => apply_xdg_spec(window, spec),
            }
            surf.spec = spec.clone();
            if surf.spec.buffer_retention == crate::surface::BufferRetention::ReleaseAfterCommit
                && !surf.pending_redraw
                && surf.pool.all_idle()
            {
                surf.pool.shrink_and_unmap();
            }
            let wl_id = surf.wl_surface_id.clone();
            surf.role.wl_surface().commit();
            #[cfg(feature = "ime")]
            if self.ime.sync_surface(&wl_id, &spec.ime) {
                self.commit_if_configured(key);
            }
            #[cfg(not(feature = "ime"))]
            let _ = wl_id;
        }
        Ok(())
    }

    pub(crate) fn destroy_surface(&mut self, key: RetainedSurfaceKey) {
        if let Some(mut surf) = self.surfaces.remove(&key) {
            self.surface_by_obj.remove(&surf.wl_surface_id);
            #[cfg(feature = "ime")]
            self.ime.clear_surface(&surf.wl_surface_id);
            if self.focused_keyboard_surface == Some(key) {
                self.focused_keyboard_surface = None;
            }
            self.active_touches.retain(|_, v| *v != key);
            // Detach buffer and drop the memfd (without truncating it) before dropping role/surface.
            let wl = surf.role.wl_surface();
            wl.attach(None, 0, 0);
            wl.commit();
            surf.pool.shrink_and_unmap();
        }
    }

    pub(crate) fn apply_input_region(
        &self,
        key: RetainedSurfaceKey,
        mode: InputMode,
        rects: &[GeoRect],
    ) {
        let Some(surf) = self.surfaces.get(&key) else {
            return;
        };
        let wl = surf.role.wl_surface();
        match mode {
            InputMode::Full => {
                wl.set_input_region(None);
            }
            InputMode::PassThrough => {
                if let Ok(region) = Region::new(&self.compositor) {
                    wl.set_input_region(Some(region.wl_region()));
                }
            }
            InputMode::Selective => {
                if let Ok(region) = Region::new(&self.compositor) {
                    for r in rects {
                        let x = r.x.0.floor() as i32;
                        let y = r.y.0.floor() as i32;
                        let w = r.width.0.ceil().max(1.0) as i32;
                        let h = r.height.0.ceil().max(1.0) as i32;
                        region.add(x, y, w, h);
                    }
                    wl.set_input_region(Some(region.wl_region()));
                }
            }
        }
    }

    pub(crate) fn commit_if_configured(&self, key: RetainedSurfaceKey) {
        if let Some(surf) = self.surfaces.get(&key)
            && surf.configured
        {
            surf.role.wl_surface().commit();
        }
    }

    pub(crate) fn configure_surface_scale(
        &self,
        key: RetainedSurfaceKey,
        logical_size: LogicalSize,
    ) {
        #[cfg(not(feature = "fractional-scale"))]
        let _ = logical_size;
        let Some(surf) = self.surfaces.get(&key) else {
            return;
        };
        let wl = surf.role.wl_surface();
        #[cfg(feature = "fractional-scale")]
        if let Some(vp) = &surf.viewport {
            wl.set_buffer_scale(1);
            let dw = logical_size.width.0.max(1.0).round() as i32;
            let dh = logical_size.height.0.max(1.0).round() as i32;
            vp.set_destination(dw, dh);
            return;
        }
        wl.set_buffer_scale(surf.scale.integer_ceil());
    }

    pub(crate) fn notify_surface_damage(
        &mut self,
        key: RetainedSurfaceKey,
        damage: &[BufferRect],
        full_redraw: bool,
    ) {
        if let Some(surf) = self.surfaces.get_mut(&key) {
            surf.pool.notify_damage(damage, full_redraw);
            surf.pending_redraw = true;
        }
    }

    pub(crate) fn present_surface_with<F>(
        &mut self,
        qh: &QueueHandle<Self>,
        key: RetainedSurfaceKey,
        logical_size: LogicalSize,
        scale: ScaleFactor,
        render_fn: F,
    ) -> Result<bool>
    where
        F: FnOnce(&mut [u8], u32, u32, u32, &[BufferRect]),
    {
        let Ok(wl_shm) = self.shm.bound_global() else {
            return Err(WaywiError::WaylandConnection(
                "wl_shm global not bound".into(),
            ));
        };
        self.configure_surface_scale(key, logical_size);

        let (buf_w, buf_h) = logical_size.to_buffer_ceil(scale);
        let Some(surf) = self.surfaces.get_mut(&key) else {
            return Ok(false);
        };
        if !surf.configured {
            surf.pending_redraw = true;
            return Ok(false);
        }

        let wl_surface = surf.role.wl_surface().clone();
        let acquire =
            surf.pool
                .acquire_for_draw(buf_w.as_u32(), buf_h.as_u32(), key, &wl_shm, qh)?;
        match acquire {
            AcquireSlot::AllBusy => {
                surf.pending_redraw = true;
                Ok(false)
            }
            AcquireSlot::Ready {
                slot,
                buffer,
                generation,
                pixels,
                stride,
                damage,
            } => {
                render_fn(pixels, buf_w.as_u32(), buf_h.as_u32(), stride, &damage);

                for rect in &damage {
                    wl_surface.damage_buffer(rect.x, rect.y, rect.width.max(1), rect.height.max(1));
                }

                if !surf.frame_pending {
                    wl_surface.frame(qh, wl_surface.clone());
                    surf.frame_pending = true;
                }

                wl_surface.attach(Some(&buffer), 0, 0);
                wl_surface.commit();

                surf.pool.mark_committed(slot, generation);
                surf.pending_redraw = false;
                Ok(true)
            }
        }
    }

    pub(crate) fn try_present_external_surface<F>(
        &mut self,
        qh: &QueueHandle<Self>,
        key: RetainedSurfaceKey,
        logical_size: LogicalSize,
        present_fn: F,
    ) -> Result<bool>
    where
        F: FnOnce(&wayland_client::protocol::wl_surface::WlSurface) -> Result<bool>,
    {
        self.configure_surface_scale(key, logical_size);
        let Some(surf) = self.surfaces.get_mut(&key) else {
            return Ok(false);
        };
        if !surf.configured {
            surf.pending_redraw = true;
            return Ok(false);
        }
        let wl_surface = surf.role.wl_surface().clone();
        if !surf.frame_pending {
            wl_surface.frame(qh, wl_surface.clone());
            surf.frame_pending = true;
        }
        if present_fn(&wl_surface)? {
            surf.pending_redraw = false;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
