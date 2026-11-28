use std::collections::{HashMap, VecDeque};

use smithay_client_toolkit::activation::ActivationState;
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
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface, wl_touch};
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};

#[cfg(feature = "fractional-scale")]
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1;
#[cfg(feature = "fractional-scale")]
use wayland_protocols::wp::viewporter::client::wp_viewporter;

use crate::capabilities::{Capabilities, Capability};
use crate::display::OutputInfo;
use crate::error::{Result, WaywiError};
use crate::geometry::{Bounds, BufferRect, LogicalSize, ScaleFactor};
use crate::input::Modifiers;
use crate::reconcile::{RetainedSurfaceKey, SurfaceSpec};
use crate::surface::{InputMode, SurfaceKey, SurfaceKind};
use crate::wayland::layer_shell::{apply_layer_spec, create_layer_surface};
use crate::wayland::output::collect_outputs;
use crate::wayland::shm_pool::{AcquireSlot, SurfaceShmPool};
use crate::wayland::surface_state::{
    ActivationRequestData, ActiveTouch, LiveWaylandSurface, SurfaceRole, WaylandEvent,
};
use crate::wayland::xdg_shell::{
    apply_xdg_spec, create_xdg_popup, create_xdg_window, reposition_xdg_popup,
};

/// Complete SCTK + Wayland protocol state dispatched from `calloop`.
pub(crate) struct WaylandState {
    pub(crate) registry_state: RegistryState,
    pub(crate) compositor: CompositorState,
    pub(crate) output_state: OutputState,
    pub(crate) seat_state: SeatState,
    pub(crate) shm: Shm,
    pub(crate) layer_shell: Option<LayerShell>,
    pub(crate) xdg_shell: Option<XdgShell>,
    pub(crate) activation: Option<ActivationState>,
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
    #[cfg(feature = "pointer-gestures")]
    pub(crate) pointer_gestures: crate::wayland::pointer_gestures::WaylandPointerGesturesState,
    #[cfg(feature = "tablet")]
    pub(crate) tablet: crate::wayland::tablet::WaylandTabletState,
    pub(crate) dnd: crate::wayland::dnd::WaylandDndState,
    pub(crate) has_xdg_decoration: bool,
    #[cfg(feature = "cursor-shape")]
    pub(crate) cursor_shape: crate::wayland::cursor_shape::WaylandCursorShapeState,
    #[cfg(feature = "session-lock")]
    pub(crate) session_lock: crate::wayland::session_lock::WaylandSessionLockState,
    #[cfg(feature = "screencopy")]
    pub(crate) screencopy: crate::wayland::screencopy::WaylandScreencopyState,
    #[cfg(feature = "foreign-toplevel")]
    pub(crate) foreign_toplevel: crate::wayland::foreign_toplevel::WaylandForeignToplevelState,
    #[cfg(feature = "ext-workspace")]
    pub(crate) ext_workspace: crate::wayland::ext_workspace::WaylandExtWorkspaceState,
    #[cfg(feature = "idle-inhibit")]
    pub(crate) idle_inhibit: crate::wayland::idle_inhibit::WaylandIdleInhibitState,
    #[cfg(feature = "virtual-keyboard")]
    pub(crate) virtual_keyboard: crate::wayland::virtual_keyboard::WaylandVirtualKeyboardState,
    #[cfg(feature = "pointer-constraints")]
    pub(crate) pointer_constraints:
        crate::wayland::pointer_constraints::WaylandPointerConstraintsState,
    #[cfg(feature = "background-effect")]
    pub(crate) background_effect: crate::wayland::background_effect::WaylandBackgroundEffectState,
    #[cfg(feature = "toplevel-icon")]
    pub(crate) toplevel_icon: crate::wayland::toplevel_icon::WaylandToplevelIconState,
    #[cfg(feature = "gamma-control")]
    pub(crate) gamma_control: crate::wayland::gamma_control::WaylandGammaControlState,
    #[cfg(feature = "color-management")]
    pub(crate) color_management: crate::wayland::color_management::WaylandColorManagementState,

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
    pub(crate) pointer_focus_surface: Option<RetainedSurfaceKey>,
    pub(crate) last_pointer_enter: Option<(wl_pointer::WlPointer, u32)>,
    pub(crate) last_applied_cursor: Option<(ObjectId, u32, crate::input::CursorShape)>,
    pub(crate) last_input_serial: Option<(wl_seat::WlSeat, u32, wl_surface::WlSurface)>,
    pub(crate) last_pointer_pos: HashMap<RetainedSurfaceKey, crate::geometry::Point>,
    pub(crate) active_touches: HashMap<i32, ActiveTouch>,
    pub(crate) modifiers: Modifiers,
    pub(crate) keyboard_layouts: Vec<String>,
    pub(crate) keyboard_layout_index: u32,

    pub(crate) events: VecDeque<WaylandEvent>,
}

impl Drop for WaylandState {
    fn drop(&mut self) {
        // 1. Clean up DnD/clipboard FD sources and surface-bound extensions BEFORE destroying wl_surfaces.
        self.dnd.cleanup();
        #[cfg(feature = "clipboard")]
        self.clipboard.cleanup();
        #[cfg(feature = "idle-inhibit")]
        self.idle_inhibit.clear_all();
        #[cfg(feature = "pointer-constraints")]
        self.pointer_constraints.clear_all_surfaces();
        #[cfg(feature = "background-effect")]
        self.background_effect.clear_all();
        #[cfg(feature = "toplevel-icon")]
        self.toplevel_icon.clear_all();
        #[cfg(feature = "gamma-control")]
        self.gamma_control.clear_all();
        #[cfg(feature = "color-management")]
        self.color_management.clear_all();
        // 2. Drop all live surfaces (including ext_session_lock_surface_v1 + wl_surface)
        //    BEFORE unlocking/destroying ext_session_lock_v1.
        self.surfaces.clear();
        self.surface_by_obj.clear();
        #[cfg(feature = "session-lock")]
        self.session_lock.maybe_unlock(false);
    }
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
        let activation = ActivationState::bind(&globals, &qh).ok();

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
        #[cfg(feature = "pointer-gestures")]
        let pointer_gestures =
            crate::wayland::pointer_gestures::WaylandPointerGesturesState::bind(&globals, &qh);
        #[cfg(feature = "tablet")]
        let tablet = crate::wayland::tablet::WaylandTabletState::bind(&globals, &qh);
        let dnd = crate::wayland::dnd::WaylandDndState::bind(&globals, &qh);
        let has_xdg_decoration = globals.contents().with_list(|list| {
            list.iter()
                .any(|g| g.interface == "zxdg_decoration_manager_v1")
        });
        #[cfg(feature = "cursor-shape")]
        let cursor_shape =
            crate::wayland::cursor_shape::WaylandCursorShapeState::bind(&globals, &qh);
        #[cfg(feature = "session-lock")]
        let session_lock =
            crate::wayland::session_lock::WaylandSessionLockState::bind(&globals, &qh);
        #[cfg(feature = "screencopy")]
        let screencopy = crate::wayland::screencopy::WaylandScreencopyState::bind(&globals, &qh);
        #[cfg(feature = "foreign-toplevel")]
        let foreign_toplevel =
            crate::wayland::foreign_toplevel::WaylandForeignToplevelState::bind(&globals, &qh);
        #[cfg(feature = "ext-workspace")]
        let ext_workspace =
            crate::wayland::ext_workspace::WaylandExtWorkspaceState::bind(&globals, &qh);
        #[cfg(feature = "idle-inhibit")]
        let idle_inhibit =
            crate::wayland::idle_inhibit::WaylandIdleInhibitState::bind(&globals, &qh);
        #[cfg(feature = "virtual-keyboard")]
        let virtual_keyboard =
            crate::wayland::virtual_keyboard::WaylandVirtualKeyboardState::bind(&globals, &qh);
        #[cfg(feature = "pointer-constraints")]
        let pointer_constraints =
            crate::wayland::pointer_constraints::WaylandPointerConstraintsState::bind(
                &globals, &qh,
            );
        #[cfg(feature = "background-effect")]
        let background_effect =
            crate::wayland::background_effect::WaylandBackgroundEffectState::bind(&globals, &qh);
        #[cfg(feature = "toplevel-icon")]
        let toplevel_icon =
            crate::wayland::toplevel_icon::WaylandToplevelIconState::bind(&globals, &qh);
        #[cfg(feature = "gamma-control")]
        let gamma_control =
            crate::wayland::gamma_control::WaylandGammaControlState::bind(&globals, &qh);
        #[cfg(feature = "color-management")]
        let color_management =
            crate::wayland::color_management::WaylandColorManagementState::bind(&globals, &qh);

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
            activation,
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
            #[cfg(feature = "pointer-gestures")]
            pointer_gestures,
            #[cfg(feature = "tablet")]
            tablet,
            dnd,
            has_xdg_decoration,
            #[cfg(feature = "cursor-shape")]
            cursor_shape,
            #[cfg(feature = "session-lock")]
            session_lock,
            #[cfg(feature = "screencopy")]
            screencopy,
            #[cfg(feature = "foreign-toplevel")]
            foreign_toplevel,
            #[cfg(feature = "ext-workspace")]
            ext_workspace,
            #[cfg(feature = "idle-inhibit")]
            idle_inhibit,
            #[cfg(feature = "virtual-keyboard")]
            virtual_keyboard,
            #[cfg(feature = "pointer-constraints")]
            pointer_constraints,
            #[cfg(feature = "background-effect")]
            background_effect,
            #[cfg(feature = "toplevel-icon")]
            toplevel_icon,
            #[cfg(feature = "gamma-control")]
            gamma_control,
            #[cfg(feature = "color-management")]
            color_management,
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
            pointer_focus_surface: None,
            last_pointer_enter: None,
            last_applied_cursor: None,
            last_input_serial: None,
            last_pointer_pos: HashMap::new(),
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
        {
            let seats: Vec<_> = state.seat_state.seats().collect();
            for seat in seats {
                state.dnd.add_seat(&seat, &qh);
                #[cfg(feature = "ime")]
                state.ime.add_seat(&seat, &qh);
                #[cfg(feature = "tablet")]
                state.tablet.add_seat(&seat, &qh);
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

    /// Clears `frame_pending` across all live surfaces (used on `OutputsChanged` so output mode,
    /// HDR/VRR, or hotplug transitions never leave a surface stalled behind a stale frame callback).
    pub(crate) fn clear_frame_pending_all(&mut self) {
        for surf in self.surfaces.values_mut() {
            surf.frame_pending = false;
        }
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

    pub(crate) fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::from_compile_features();
        caps.set(Capability::Wayland, true);
        caps.set(Capability::LayerShell, self.layer_shell.is_some());
        caps.set(Capability::XdgShell, self.xdg_shell.is_some());
        caps.set(Capability::XdgActivation, self.activation.is_some());
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
        caps.set(
            Capability::KeyboardLayout,
            self.has_keyboard && !self.keyboard_layouts.is_empty(),
        );
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
        #[cfg(feature = "pointer-gestures")]
        caps.set(
            Capability::PointerGestures,
            self.pointer_gestures.is_supported(),
        );
        #[cfg(not(feature = "pointer-gestures"))]
        caps.set(Capability::PointerGestures, false);
        #[cfg(feature = "tablet")]
        caps.set(Capability::TabletInput, self.tablet.is_supported());
        #[cfg(not(feature = "tablet"))]
        caps.set(Capability::TabletInput, false);
        caps.set(Capability::DragAndDrop, self.dnd.is_supported());
        caps.set(
            Capability::XdgDecoration,
            self.xdg_shell.is_some() && self.has_xdg_decoration,
        );
        #[cfg(feature = "cursor-shape")]
        caps.set(Capability::CursorShape, self.cursor_shape.is_supported());
        #[cfg(not(feature = "cursor-shape"))]
        caps.set(Capability::CursorShape, false);
        #[cfg(feature = "session-lock")]
        caps.set(Capability::SessionLock, self.session_lock.is_supported());
        #[cfg(not(feature = "session-lock"))]
        caps.set(Capability::SessionLock, false);
        #[cfg(feature = "screencopy")]
        caps.set(Capability::Screencopy, self.screencopy.is_supported());
        #[cfg(not(feature = "screencopy"))]
        caps.set(Capability::Screencopy, false);
        #[cfg(feature = "foreign-toplevel")]
        {
            caps.set(
                Capability::ForeignToplevel,
                self.foreign_toplevel.is_supported(),
            );
            caps.set(
                Capability::ForeignToplevelControl,
                self.foreign_toplevel.can_control(),
            );
        }
        #[cfg(not(feature = "foreign-toplevel"))]
        {
            caps.set(Capability::ForeignToplevel, false);
            caps.set(Capability::ForeignToplevelControl, false);
        }
        #[cfg(feature = "ext-workspace")]
        caps.set(Capability::ExtWorkspace, self.ext_workspace.is_supported());
        #[cfg(not(feature = "ext-workspace"))]
        caps.set(Capability::ExtWorkspace, false);
        #[cfg(feature = "idle-inhibit")]
        caps.set(Capability::IdleInhibit, self.idle_inhibit.is_supported());
        #[cfg(not(feature = "idle-inhibit"))]
        caps.set(Capability::IdleInhibit, false);
        #[cfg(feature = "virtual-keyboard")]
        caps.set(
            Capability::VirtualKeyboard,
            self.virtual_keyboard.is_supported(),
        );
        #[cfg(not(feature = "virtual-keyboard"))]
        caps.set(Capability::VirtualKeyboard, false);
        #[cfg(feature = "pointer-constraints")]
        {
            caps.set(
                Capability::PointerConstraints,
                self.pointer_constraints.supports_constraints(),
            );
            caps.set(
                Capability::RelativePointer,
                self.pointer_constraints.supports_relative(),
            );
        }
        #[cfg(not(feature = "pointer-constraints"))]
        {
            caps.set(Capability::PointerConstraints, false);
            caps.set(Capability::RelativePointer, false);
        }
        #[cfg(feature = "background-effect")]
        caps.set(
            Capability::BackgroundEffect,
            self.background_effect.is_supported(),
        );
        #[cfg(not(feature = "background-effect"))]
        caps.set(Capability::BackgroundEffect, false);
        #[cfg(feature = "toplevel-icon")]
        caps.set(Capability::ToplevelIcon, self.toplevel_icon.is_supported());
        #[cfg(not(feature = "toplevel-icon"))]
        caps.set(Capability::ToplevelIcon, false);
        #[cfg(feature = "gamma-control")]
        caps.set(Capability::GammaControl, self.gamma_control.is_supported());
        #[cfg(not(feature = "gamma-control"))]
        caps.set(Capability::GammaControl, false);
        #[cfg(feature = "color-management")]
        caps.set(
            Capability::ColorManagement,
            self.color_management.is_supported(),
        );
        #[cfg(not(feature = "color-management"))]
        caps.set(Capability::ColorManagement, false);
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
            Capability::AudioCapture,
            caps.has(Capability::PipeWire)
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
            cfg!(feature = "dbus") && dbus_socket_available(),
        );
        caps
    }

    /// Returns only the Wayland-protocol/seat capability bits for live `OutputsChanged` refresh
    /// without clobbering runtime-probed IPC, GPU, audio, or accessibility flags.
    pub(crate) fn refresh_wayland_capabilities(&self, target: &mut Capabilities) {
        let live = self.capabilities();
        target.refresh_wayland_from(&live);
    }

    /// Requests a Wayland `xdg_activation_v1` focus-handoff token asynchronously.
    ///
    /// Returns `false` if the compositor does not advertise `xdg_activation_v1`.
    pub(crate) fn request_activation_token(
        &self,
        qh: &QueueHandle<Self>,
        request_id: u64,
        app_id: Option<String>,
    ) -> bool {
        let Some(ref activation) = self.activation else {
            return false;
        };
        let (seat_and_serial, surface) = match &self.last_input_serial {
            Some((seat, serial, surf)) => (Some((seat.clone(), *serial)), Some(surf.clone())),
            None => {
                let fallback_surf = self
                    .focused_keyboard_surface
                    .and_then(|rk| self.surfaces.get(&rk))
                    .map(|s| s.role.wl_surface().clone());
                (None, fallback_surf)
            }
        };
        activation.request_token_with_data(
            qh,
            ActivationRequestData {
                request_id,
                app_id,
                seat_and_serial,
                surface,
            },
        );
        true
    }

    /// Activates the live Wayland surface matching `surface_key` with `token`.
    pub(crate) fn activate_surface(&self, surface_key: SurfaceKey, token: String) -> bool {
        let Some(ref activation) = self.activation else {
            return false;
        };
        let Some((_, surf)) = self
            .surfaces
            .iter()
            .find(|(rk, _)| rk.surface_key == surface_key)
        else {
            return false;
        };
        activation.activate::<Self>(surf.role.wl_surface(), token);
        true
    }

    pub(crate) fn create_surface(
        &mut self,
        qh: &QueueHandle<Self>,
        key: RetainedSurfaceKey,
        spec: &SurfaceSpec,
    ) -> Result<()> {
        if self.surfaces.contains_key(&key) {
            self.update_surface_spec(qh, key, spec)?;
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
            SurfaceKind::Popup => {
                let xdg = self
                    .xdg_shell
                    .as_ref()
                    .ok_or(WaywiError::UnsupportedCapability(Capability::XdgShell))?;
                let popup_spec = spec.popup.ok_or_else(|| {
                    WaywiError::SurfaceError("SurfaceKind::Popup missing popup spec".to_owned())
                })?;
                let exact_parent = RetainedSurfaceKey {
                    surface_key: popup_spec.parent,
                    output_id: key.output_id,
                };
                let parent_surf = self
                    .surfaces
                    .get(&exact_parent)
                    .or_else(|| {
                        self.surfaces
                            .iter()
                            .find(|(rk, _)| rk.surface_key == popup_spec.parent)
                            .map(|(_, s)| s)
                    })
                    .ok_or_else(|| {
                        WaywiError::SurfaceError(
                            "xdg_popup parent surface is not currently mapped".to_owned(),
                        )
                    })?;
                let grab = self
                    .last_input_serial
                    .as_ref()
                    .map(|(seat, serial, _)| (seat, *serial));
                SurfaceRole::Popup(create_xdg_popup(
                    xdg,
                    qh,
                    wl_surface,
                    spec,
                    &parent_surf.role,
                    grab,
                )?)
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
            SurfaceKind::SessionLock => {
                #[cfg(feature = "session-lock")]
                {
                    let out = output_obj.cloned().ok_or_else(|| {
                        WaywiError::SurfaceError(
                            "ext_session_lock_v1 requires a concrete wl_output".to_owned(),
                        )
                    })?;
                    SurfaceRole::SessionLock(
                        self.session_lock
                            .create_lock_surface(qh, key, wl_surface, &out)?,
                    )
                }
                #[cfg(not(feature = "session-lock"))]
                {
                    return Err(WaywiError::UnsupportedCapability(Capability::SessionLock));
                }
            }
        };

        #[cfg(feature = "idle-inhibit")]
        if spec.inhibit_idle {
            self.idle_inhibit.sync_surface(role.wl_surface(), true, qh);
        }
        #[cfg(feature = "pointer-constraints")]
        if spec.pointer_constraint != crate::input::PointerConstraintMode::None {
            let ptr = self.pointers.first();
            self.pointer_constraints.sync_surface(
                role.wl_surface(),
                spec.pointer_constraint,
                ptr,
                qh,
            );
        }
        #[cfg(feature = "background-effect")]
        if spec.background_blur != crate::surface::BackgroundBlur::None {
            self.background_effect.sync_surface(
                &self.compositor,
                role.wl_surface(),
                spec.background_blur,
                spec.logical_size,
                qh,
            );
        }
        #[cfg(feature = "color-management")]
        if spec.color_space != crate::surface::ColorSpace::Default {
            self.color_management
                .sync_surface(role.wl_surface(), spec.color_space, qh);
        }

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
                gpu_bound: false,
                reposition_token: 0,
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
        qh: &QueueHandle<Self>,
        key: RetainedSurfaceKey,
        spec: &SurfaceSpec,
    ) -> Result<()> {
        let Some(surf) = self.surfaces.get_mut(&key) else {
            return Ok(());
        };
        if surf.spec != *spec {
            let deco_changed = surf.spec.decoration != spec.decoration;
            match &surf.role {
                SurfaceRole::Layer(layer) => apply_layer_spec(layer, spec),
                SurfaceRole::Xdg(window) => apply_xdg_spec(window, spec, deco_changed),
                SurfaceRole::Popup(popup) => {
                    if (surf.spec.popup != spec.popup
                        || surf.spec.logical_size != spec.logical_size)
                        && let Some(ref xdg) = self.xdg_shell
                    {
                        surf.reposition_token = surf.reposition_token.wrapping_add(1);
                        reposition_xdg_popup(xdg, popup, spec, surf.reposition_token)?;
                    }
                }
                #[cfg(feature = "session-lock")]
                SurfaceRole::SessionLock(_) => {}
            }
            let wl_surface = surf.role.wl_surface().clone();
            let wl_id = surf.wl_surface_id.clone();
            #[cfg(feature = "session-lock")]
            let is_unconfigured_lock =
                matches!(surf.role, SurfaceRole::SessionLock(_)) && !surf.configured;
            #[cfg(not(feature = "session-lock"))]
            let is_unconfigured_lock = false;
            surf.spec = spec.clone();
            if surf.spec.buffer_retention == crate::surface::BufferRetention::ReleaseAfterCommit
                && !surf.pending_redraw
                && surf.pool.all_idle()
            {
                surf.pool.shrink_and_unmap();
            }
            #[cfg(feature = "idle-inhibit")]
            self.idle_inhibit
                .sync_surface(&wl_surface, spec.inhibit_idle, qh);
            #[cfg(feature = "pointer-constraints")]
            {
                let ptr = self.pointers.first();
                self.pointer_constraints.sync_surface(
                    &wl_surface,
                    spec.pointer_constraint,
                    ptr,
                    qh,
                );
            }
            #[cfg(feature = "background-effect")]
            self.background_effect.sync_surface(
                &self.compositor,
                &wl_surface,
                spec.background_blur,
                spec.logical_size,
                qh,
            );
            #[cfg(feature = "color-management")]
            self.color_management
                .sync_surface(&wl_surface, spec.color_space, qh);
            if !is_unconfigured_lock {
                wl_surface.commit();
            }
            #[cfg(feature = "ime")]
            if self.ime.sync_surface(&wl_id, &spec.ime) && !is_unconfigured_lock {
                self.commit_if_configured(key);
            }
            #[cfg(not(feature = "ime"))]
            let _ = wl_id;
            #[cfg(not(any(
                feature = "idle-inhibit",
                feature = "pointer-constraints",
                feature = "background-effect",
                feature = "color-management"
            )))]
            let _ = qh;
        }
        Ok(())
    }

    pub(crate) fn destroy_surface(&mut self, key: RetainedSurfaceKey, detach_null_buffer: bool) {
        if let Some(mut surf) = self.surfaces.remove(&key) {
            self.surface_by_obj.remove(&surf.wl_surface_id);
            self.dnd.clear_surface(key);
            #[cfg(feature = "ime")]
            self.ime.clear_surface(&surf.wl_surface_id);
            #[cfg(feature = "pointer-gestures")]
            self.pointer_gestures.clear_surface(key);
            #[cfg(feature = "tablet")]
            self.tablet.clear_surface(&surf.wl_surface_id);
            #[cfg(feature = "idle-inhibit")]
            self.idle_inhibit.clear_surface(&surf.wl_surface_id);
            #[cfg(feature = "pointer-constraints")]
            self.pointer_constraints.clear_surface(&surf.wl_surface_id);
            #[cfg(feature = "background-effect")]
            self.background_effect.clear_surface(&surf.wl_surface_id);
            #[cfg(feature = "toplevel-icon")]
            if let SurfaceRole::Xdg(ref window) = surf.role {
                self.toplevel_icon
                    .clear_toplevel(&window.xdg_toplevel().id());
            }
            #[cfg(feature = "color-management")]
            self.color_management.clear_surface(&surf.wl_surface_id);
            if self.focused_keyboard_surface == Some(key) {
                self.focused_keyboard_surface = None;
            }
            if self.pointer_focus_surface == Some(key) {
                self.pointer_focus_surface = None;
                self.last_pointer_enter = None;
                self.last_applied_cursor = None;
            }
            if self
                .last_input_serial
                .as_ref()
                .is_some_and(|(_, _, s)| s.id() == surf.wl_surface_id)
            {
                self.last_input_serial = None;
            }
            self.last_pointer_pos.remove(&key);
            self.active_touches.retain(|_, v| v.key != key);
            // Detach buffer for SHM toplevel/layer surfaces before dropping; skip null-buffer
            // commits for xdg_popup, session-lock surfaces (null_buffer error), compositor-closed
            // layers, or GPU-bound surfaces.
            #[cfg(feature = "session-lock")]
            let is_session_lock = matches!(surf.role, SurfaceRole::SessionLock(_));
            #[cfg(not(feature = "session-lock"))]
            let is_session_lock = false;
            if detach_null_buffer
                && !surf.gpu_bound
                && !is_session_lock
                && !matches!(surf.role, SurfaceRole::Popup(_))
            {
                let wl = surf.role.wl_surface();
                wl.attach(None, 0, 0);
                wl.commit();
            }
            surf.pool.shrink_and_unmap();
            drop(surf);
            #[cfg(feature = "session-lock")]
            if is_session_lock {
                let any_remaining = self
                    .surfaces
                    .values()
                    .any(|s| matches!(s.role, SurfaceRole::SessionLock(_)));
                self.session_lock.maybe_unlock(any_remaining);
            }
        }
    }

    /// Applies a cursor shape (`wp_cursor_shape_v1` or `CursorShape::Hidden`) using the active
    /// pointer enter serial, skipping redundant requests uniformly with or without `cursor-shape`.
    pub(crate) fn apply_cursor_shape(&mut self, shape: crate::input::CursorShape) -> bool {
        let Some((ref pointer, serial)) = self.last_pointer_enter else {
            return false;
        };
        let pid = pointer.id();
        if self.last_applied_cursor == Some((pid.clone(), serial, shape)) {
            return false;
        }
        let applied = if shape == crate::input::CursorShape::Hidden {
            pointer.set_cursor(serial, None, 0, 0);
            true
        } else {
            #[cfg(feature = "cursor-shape")]
            {
                self.cursor_shape.set_shape(pointer, serial, shape)
            }
            #[cfg(not(feature = "cursor-shape"))]
            {
                false
            }
        };
        if applied {
            self.last_applied_cursor = Some((pid, serial, shape));
        }
        applied
    }

    /// Starts an async screencopy request targeting an output or output sub-region.
    #[cfg(feature = "screencopy")]
    pub(crate) fn start_screencopy(
        &mut self,
        qh: &QueueHandle<Self>,
        request_id: u64,
        target: &crate::wayland_ext::ScreencopyTarget,
        overlay_cursor: bool,
    ) -> Result<()> {
        let (out_sel, region) = match target {
            crate::wayland_ext::ScreencopyTarget::Output(o) => (o, None),
            crate::wayland_ext::ScreencopyTarget::Region { output, bounds } => {
                (output, Some(*bounds))
            }
        };
        let resolved = crate::display::resolve_outputs(out_sel, &self.outputs);
        let out_info = resolved
            .first()
            .copied()
            .or_else(|| self.outputs.first())
            .ok_or_else(|| {
                WaywiError::SurfaceError("screencopy: no Wayland output available".into())
            })?;
        let wl_out = self
            .output_handles
            .get(&out_info.id)
            .cloned()
            .ok_or_else(|| WaywiError::SurfaceError("screencopy: output handle missing".into()))?;
        self.screencopy.start_capture(
            request_id,
            &wl_out,
            region,
            out_info.logical_size,
            overlay_cursor,
            qh,
        )
    }

    #[cfg(feature = "foreign-toplevel")]
    pub(crate) fn apply_foreign_toplevel_action(
        &self,
        action: &crate::effect::ForeignToplevelAction,
    ) -> bool {
        let seat = self.seat_state.seats().next();
        self.foreign_toplevel
            .apply_action(action, seat.as_ref(), &self.output_handles)
    }

    #[cfg(feature = "ext-workspace")]
    pub(crate) fn apply_ext_workspace_action(
        &self,
        action: &crate::effect::ExtWorkspaceAction,
    ) -> bool {
        self.ext_workspace.apply_action(action)
    }

    #[cfg(feature = "ext-workspace")]
    pub(crate) fn commit_ext_workspace(&self) {
        self.ext_workspace.commit();
    }

    #[cfg(feature = "virtual-keyboard")]
    pub(crate) fn apply_virtual_keyboard_action(
        &mut self,
        qh: &QueueHandle<Self>,
        action: &crate::effect::VirtualKeyboardAction,
    ) -> Result<()> {
        let seat = self.seat_state.seats().next();
        match action {
            crate::effect::VirtualKeyboardAction::Key { keycode, state } => self
                .virtual_keyboard
                .send_key(seat.as_ref(), qh, *keycode, *state),
            crate::effect::VirtualKeyboardAction::Modifiers {
                depressed,
                latched,
                locked,
                group,
            } => self.virtual_keyboard.send_modifiers(
                seat.as_ref(),
                qh,
                *depressed,
                *latched,
                *locked,
                *group,
            ),
            crate::effect::VirtualKeyboardAction::Keymap(km) => {
                self.virtual_keyboard.upload_keymap(seat.as_ref(), qh, km)
            }
        }
    }

    pub(crate) fn start_drag(
        &mut self,
        qh: &QueueHandle<Self>,
        req: crate::effect::DragStartRequest,
    ) -> bool {
        let Some((seat, serial, _)) = self.last_input_serial.clone() else {
            return false;
        };
        let Some(wl_surf) = self
            .surfaces
            .iter()
            .find(|(rk, _)| rk.surface_key == req.surface_key)
            .map(|(_, s)| s.role.wl_surface().clone())
        else {
            return false;
        };
        self.dnd
            .start_drag(&wl_surf, &seat, serial, req.entries, req.actions, qh)
    }

    #[cfg(feature = "toplevel-icon")]
    pub(crate) fn sync_toplevel_icon(
        &mut self,
        qh: &QueueHandle<Self>,
        key: RetainedSurfaceKey,
        assets: &crate::asset::AssetCache,
    ) {
        let Some(surf) = self.surfaces.get(&key) else {
            return;
        };
        if let SurfaceRole::Xdg(ref window) = surf.role {
            self.toplevel_icon.sync_toplevel(
                window.xdg_toplevel(),
                surf.spec.window_icon.as_ref(),
                &self.shm,
                assets,
                qh,
            );
        }
    }

    #[cfg(feature = "gamma-control")]
    pub(crate) fn apply_gamma_action(
        &mut self,
        qh: &QueueHandle<Self>,
        action: &crate::effect::GammaAction,
    ) -> bool {
        self.gamma_control
            .apply_action(action, &self.outputs, &self.output_handles, qh)
    }

    pub(crate) fn apply_input_region(
        &self,
        key: RetainedSurfaceKey,
        mode: InputMode,
        rects: &[Bounds],
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

    /// Arms a VSync `wl_surface.frame` callback on `key` when configured and not already armed,
    /// returning `true` if a new frame callback was requested.
    pub(crate) fn arm_frame_callback(
        &mut self,
        qh: &QueueHandle<Self>,
        key: RetainedSurfaceKey,
    ) -> bool {
        let Some(surf) = self.surfaces.get_mut(&key) else {
            return false;
        };
        if !surf.configured || surf.frame_pending {
            return false;
        }
        let wl = surf.role.wl_surface();
        wl.frame(qh, wl.clone());
        wl.commit();
        surf.frame_pending = true;
        true
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

    pub(crate) fn trim_idle_shm_pools(&mut self) {
        for surf in self.surfaces.values_mut() {
            if surf.spec.buffer_retention == crate::surface::BufferRetention::ReleaseAfterCommit
                && !surf.pending_redraw
                && surf.pool.all_idle()
            {
                surf.pool.shrink_and_unmap();
            } else {
                surf.pool.trim_idle_slots();
            }
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
        F: FnOnce(&mut [u8], u32, u32, u32, &[BufferRect]) -> Result<()>,
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
                render_fn(pixels, buf_w.as_u32(), buf_h.as_u32(), stride, &damage)?;

                for rect in &damage {
                    wl_surface.damage_buffer(rect.x, rect.y, rect.width.max(1), rect.height.max(1));
                }

                if !surf.frame_pending {
                    wl_surface.frame(qh, wl_surface.clone());
                    surf.frame_pending = true;
                }

                #[cfg(feature = "session-lock")]
                if let SurfaceRole::SessionLock(ref mut sl) = surf.role {
                    sl.ack_pending_configure();
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
    ) -> Result<crate::render::WaylandPresentOutcome>
    where
        F: FnOnce(
            &wayland_client::protocol::wl_surface::WlSurface,
            &mut dyn FnMut(),
        ) -> Result<crate::render::WaylandPresentOutcome>,
    {
        use crate::render::WaylandPresentOutcome;

        self.configure_surface_scale(key, logical_size);
        let Some(surf) = self.surfaces.get_mut(&key) else {
            return Ok(WaylandPresentOutcome::Deferred);
        };
        if !surf.configured || surf.frame_pending {
            surf.pending_redraw = true;
            return Ok(WaylandPresentOutcome::Deferred);
        }
        let wl_surface = surf.role.wl_surface().clone();
        let was_gpu_bound = surf.gpu_bound;
        let mut armed_frame = false;
        let mut arm_frame_callback = || {
            if !armed_frame {
                #[cfg(feature = "session-lock")]
                if let SurfaceRole::SessionLock(ref mut sl) = surf.role {
                    sl.ack_pending_configure();
                }
                wl_surface.frame(qh, wl_surface.clone());
                armed_frame = true;
            }
        };
        let outcome = present_fn(&wl_surface, &mut arm_frame_callback)?;
        let Some(surf) = self.surfaces.get_mut(&key) else {
            return Ok(outcome);
        };
        match outcome {
            WaylandPresentOutcome::Committed => {
                surf.gpu_bound = true;
                surf.pending_redraw = false;
                if armed_frame {
                    surf.frame_pending = true;
                }
                Ok(WaylandPresentOutcome::Committed)
            }
            WaylandPresentOutcome::Deferred => {
                surf.gpu_bound = true;
                surf.pending_redraw = true;
                Ok(WaylandPresentOutcome::Deferred)
            }
            WaylandPresentOutcome::FallbackToShm => {
                if was_gpu_bound {
                    // Never fall back to `wl_shm` on a `wl_surface` that already bound a GPU
                    // swapchain; keep waiting until swapchain recovery or surface recreation.
                    surf.pending_redraw = true;
                    Ok(WaylandPresentOutcome::Deferred)
                } else {
                    Ok(WaylandPresentOutcome::FallbackToShm)
                }
            }
        }
    }
}

fn dbus_socket_available() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        || std::env::var_os("DBUS_SYSTEM_BUS_ADDRESS").is_some()
        || std::path::Path::new("/run/dbus/system_bus_socket").exists()
    {
        return true;
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR")
        && std::path::Path::new(&runtime_dir).join("bus").exists()
    {
        return true;
    }
    false
}
