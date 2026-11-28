use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crate::capabilities::{Capabilities, Capability};
use crate::display::{Output, OutputId, OutputInfo};
use crate::effect::{
    DragStartRequest, ExtWorkspaceAction, ForeignToplevelAction, GammaAction, VirtualKeyboardAction,
};
use crate::error::{Result, WaywiError};
use crate::geometry::{LogicalPx, LogicalSize, Point};
use crate::input::{
    CursorShape, DndAction, DndEvent, GestureEvent, ImeConfig, ImeEvent, KeyEvent, Modifiers,
    MouseButton, PointerConstraintMode, RelativePointerEvent, ScrollEvent, ScrollSource,
    TabletEvent, TouchEvent, VirtualKeyState,
};
use crate::reconcile::RetainedSurfaceKey;
use crate::render::{FrameScene, RenderBackend, RendererPreference};
use crate::surface::{BackgroundBlur, ColorSpace, SurfaceKey, WindowDecoration, WindowIcon};
use crate::text::SharedStr;
use crate::wayland_ext::{
    ExtWorkspaceGroupId, ExtWorkspaceId, ForeignToplevelId, GammaRamp, ScreencopyFrame,
};

use super::core::{CoreEngine, EffectPostWork};
use super::memory::MemoryConfig;
use super::runner::Application;
use super::subscription_mgr::flatten_specs;

/// Foreign-toplevel control action recorded by [`HeadlessRuntime`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadlessToplevelAction {
    Activate(ForeignToplevelId),
    Close(ForeignToplevelId),
    SetMinimized(ForeignToplevelId, bool),
    SetMaximized(ForeignToplevelId, bool),
    SetFullscreen(ForeignToplevelId, bool, Option<OutputId>),
}

/// `ext_workspace_v1` mutation recorded by [`HeadlessRuntime`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadlessExtWorkspaceAction {
    Activate(ExtWorkspaceId),
    Deactivate(ExtWorkspaceId),
    Create {
        group: ExtWorkspaceGroupId,
        name: String,
    },
    Remove(ExtWorkspaceId),
}

/// `zwp_virtual_keyboard_v1` action recorded by [`HeadlessRuntime`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadlessVirtualKeyboardAction {
    Key {
        keycode: u32,
        state: VirtualKeyState,
    },
    Modifiers {
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    },
    Keymap(String),
}

/// Outgoing Drag & Drop request recorded by [`HeadlessRuntime`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessDragStart {
    pub surface_key: SurfaceKey,
    pub entries: Vec<(String, Vec<u8>)>,
    pub actions: DndAction,
}

/// `zwlr_gamma_control_manager_v1` action recorded by [`HeadlessRuntime`].
#[derive(Debug, Clone, PartialEq)]
pub enum HeadlessGammaAction {
    SetRamp {
        output: Output,
        ramp: GammaRamp,
    },
    SetTemperature {
        output: Output,
        kelvin: u32,
        brightness: f32,
    },
    Reset {
        output: Output,
    },
}

/// Deterministic headless runtime driving any [`Application`] without opening a Wayland socket.
///
/// Designed for unit/integration tests and Criterion event-dispatch benchmarks verifying view
/// reconciliation, memo skipping, zero steady-state arena leaks, hit-testing, timers, memory
/// trimming, and rendering pipelines cleanly.
pub struct HeadlessRuntime<A: Application> {
    state: A::State,
    outputs: Vec<OutputInfo>,
    core: CoreEngine<A::Message>,
    inbox: VecDeque<A::Message>,
    frame_buffers: HashMap<RetainedSurfaceKey, Vec<u8>>,
    activation_seq: u64,
    activated_surfaces: Vec<(SurfaceKey, String)>,
    synthetic_screencopy: Option<ScreencopyFrame>,
    recorded_toplevel_actions: Vec<HeadlessToplevelAction>,
    recorded_ext_workspace_actions: Vec<HeadlessExtWorkspaceAction>,
    recorded_virtual_keyboard_actions: Vec<HeadlessVirtualKeyboardAction>,
    recorded_drag_starts: Vec<HeadlessDragStart>,
    recorded_gamma_actions: Vec<HeadlessGammaAction>,
    #[cfg(feature = "accessibility")]
    a11y_tree_state: crate::accessibility::AccessibilityTreeState,
}

impl<A: Application> HeadlessRuntime<A> {
    /// Boots `A` against a synthetic output list and explicit [`Capabilities`].
    pub fn new(capabilities: Capabilities, outputs: Vec<OutputInfo>) -> Result<Self> {
        Self::with_memory(
            capabilities,
            outputs,
            RendererPreference::Auto,
            MemoryConfig::default(),
        )
    }

    /// Boots `A` against synthetic outputs using an explicit [`RendererPreference`].
    pub fn new_with_renderer(
        capabilities: Capabilities,
        outputs: Vec<OutputInfo>,
        renderer_pref: RendererPreference,
    ) -> Result<Self> {
        Self::with_memory(
            capabilities,
            outputs,
            renderer_pref,
            MemoryConfig::default(),
        )
    }

    /// Boots `A` against synthetic outputs using an explicit [`RendererPreference`] and
    /// [`MemoryConfig`].
    pub fn with_memory(
        capabilities: Capabilities,
        outputs: Vec<OutputInfo>,
        renderer_pref: RendererPreference,
        memory: MemoryConfig,
    ) -> Result<Self> {
        let mut core = CoreEngine::new(
            capabilities,
            renderer_pref,
            None,
            memory,
            SharedStr::from_static("waywi-headless"),
            false,
        )?;
        let now = Instant::now();
        let (init_res, post) = core.run_effect(
            &outputs,
            #[cfg(feature = "clipboard")]
            None,
            now,
            |ctx| A::init(ctx),
        );
        let state = init_res?;

        let mut rt = Self {
            state,
            outputs,
            core,
            inbox: VecDeque::new(),
            frame_buffers: HashMap::new(),
            activation_seq: 0,
            activated_surfaces: Vec::new(),
            synthetic_screencopy: None,
            recorded_toplevel_actions: Vec::new(),
            recorded_ext_workspace_actions: Vec::new(),
            recorded_virtual_keyboard_actions: Vec::new(),
            recorded_drag_starts: Vec::new(),
            recorded_gamma_actions: Vec::new(),
            #[cfg(feature = "accessibility")]
            a11y_tree_state: crate::accessibility::AccessibilityTreeState::default(),
        };
        rt.drain_post_work(post);
        rt.step_at(now)?;
        Ok(rt)
    }

    fn drain_post_work(&mut self, post: EffectPostWork<A::Message>) {
        self.inbox.extend(post.inline_messages);
        let supported = self.core.capabilities.has(Capability::XdgActivation);
        for (_app_id, callback) in post.activation_requests {
            let result = if supported {
                self.activation_seq = self.activation_seq.saturating_add(1);
                Ok(format!("headless-activation-token-{}", self.activation_seq))
            } else {
                Err(WaywiError::UnsupportedCapability(Capability::XdgActivation))
            };
            self.inbox.push_back(callback(result));
        }
        if supported {
            self.activated_surfaces.extend(post.surface_activations);
        }

        let sc_supported = self.core.capabilities.has(Capability::Screencopy);
        for (_target, _overlay_cursor, callback) in post.screencopy_requests {
            let result = if !sc_supported {
                Err(WaywiError::UnsupportedCapability(Capability::Screencopy))
            } else if let Some(frame) = self.synthetic_screencopy.clone() {
                Ok(frame)
            } else {
                Err(WaywiError::InvalidState(
                    "no synthetic screencopy frame configured on HeadlessRuntime; call set_synthetic_screencopy(...)"
                        .to_owned(),
                ))
            };
            self.inbox.push_back(callback(result));
        }

        if self
            .core
            .capabilities
            .has(Capability::ForeignToplevelControl)
        {
            for action in post.toplevel_actions {
                let recorded = match action {
                    ForeignToplevelAction::Activate(id) => HeadlessToplevelAction::Activate(id),
                    ForeignToplevelAction::Close(id) => HeadlessToplevelAction::Close(id),
                    ForeignToplevelAction::SetMinimized(id, m) => {
                        HeadlessToplevelAction::SetMinimized(id, m)
                    }
                    ForeignToplevelAction::SetMaximized(id, m) => {
                        HeadlessToplevelAction::SetMaximized(id, m)
                    }
                    ForeignToplevelAction::SetFullscreen(id, f, out) => {
                        HeadlessToplevelAction::SetFullscreen(id, f, out)
                    }
                };
                self.recorded_toplevel_actions.push(recorded);
            }
        }

        if self.core.capabilities.has(Capability::ExtWorkspace) {
            for action in post.ext_workspace_actions {
                let recorded = match action {
                    ExtWorkspaceAction::Activate(id) => HeadlessExtWorkspaceAction::Activate(id),
                    ExtWorkspaceAction::Deactivate(id) => {
                        HeadlessExtWorkspaceAction::Deactivate(id)
                    }
                    ExtWorkspaceAction::Create { group, name } => {
                        HeadlessExtWorkspaceAction::Create { group, name }
                    }
                    ExtWorkspaceAction::Remove(id) => HeadlessExtWorkspaceAction::Remove(id),
                };
                self.recorded_ext_workspace_actions.push(recorded);
            }
        }

        if self.core.capabilities.has(Capability::VirtualKeyboard) {
            for action in post.virtual_keyboard_actions {
                let recorded = match action {
                    VirtualKeyboardAction::Key { keycode, state } => {
                        HeadlessVirtualKeyboardAction::Key { keycode, state }
                    }
                    VirtualKeyboardAction::Modifiers {
                        depressed,
                        latched,
                        locked,
                        group,
                    } => HeadlessVirtualKeyboardAction::Modifiers {
                        depressed,
                        latched,
                        locked,
                        group,
                    },
                    VirtualKeyboardAction::Keymap(km) => HeadlessVirtualKeyboardAction::Keymap(km),
                };
                self.recorded_virtual_keyboard_actions.push(recorded);
            }
        }

        if self.core.capabilities.has(Capability::DragAndDrop) {
            for DragStartRequest {
                surface_key,
                entries,
                actions,
            } in post.drag_starts
            {
                self.recorded_drag_starts.push(HeadlessDragStart {
                    surface_key,
                    entries,
                    actions,
                });
            }
        }

        if self.core.capabilities.has(Capability::GammaControl) {
            for action in post.gamma_actions {
                let recorded = match action {
                    GammaAction::SetRamp { output, ramp } => {
                        HeadlessGammaAction::SetRamp { output, ramp }
                    }
                    GammaAction::SetTemperature {
                        output,
                        kelvin,
                        brightness,
                    } => HeadlessGammaAction::SetTemperature {
                        output,
                        kelvin,
                        brightness,
                    },
                    GammaAction::Reset { output } => HeadlessGammaAction::Reset { output },
                };
                self.recorded_gamma_actions.push(recorded);
            }
        }
    }

    pub fn enqueue(&mut self, msg: A::Message) {
        self.inbox.push_back(msg);
    }

    /// Immediately trims all reclaimable caches and transient buffers in the headless engine.
    pub fn trim_memory(&mut self) {
        self.core.trim_memory();
    }

    pub fn step(&mut self) -> Result<()> {
        self.step_at(Instant::now())
    }

    pub fn step_at(&mut self, now: Instant) -> Result<()> {
        let _ = self.core.collect_core_due(now, &mut self.inbox);

        while let Some(msg) = self.inbox.pop_front() {
            let (res, post) = self.core.run_effect(
                &self.outputs,
                #[cfg(feature = "clipboard")]
                None,
                now,
                |ctx| A::update(&mut self.state, msg, ctx),
            );
            res?;
            self.drain_post_work(post);
        }

        let report = {
            let _arena_guard = self.core.arena.enter_scope();
            let subs = A::subscriptions(&self.state);
            let mut flat = Vec::new();
            flatten_specs(&subs, &mut flat);
            self.core.timers.reconcile(flat.iter().copied(), now);
            self.core
                .system_samplers
                .reconcile(flat.iter().copied(), &mut self.core.system);
            for spec in &flat {
                if let crate::subscription::SubscriptionSpec::AnimationFrame { map, .. } = spec {
                    self.inbox.push_back(map.call(now));
                }
            }

            self.core
                .reconcile_view(&self.outputs, now, || A::view(&self.state))
        };

        for rk in &report.removed_surfaces {
            self.frame_buffers.remove(rk);
            self.core.renderer.drop_surface_target(*rk);
            self.core.configured_sizes.remove(rk);
            self.core.surface_scales.remove(rk);
        }

        self.render_dirty_surfaces()?;

        Ok(())
    }

    /// Advances active sprite timelines and retained element transitions up to `now` and re-renders
    /// any dirtied surfaces **without** calling `Application::view(state)`.
    ///
    /// Returns `true` if any surface still has active transitions or dirtied pixels.
    pub fn tick_animations_at(&mut self, now: Instant) -> Result<bool> {
        let (dirty, _, msgs) = self.core.reconciler.tick_animations(now);
        self.inbox.extend(msgs);
        let had_dirty = !dirty.is_empty();
        self.render_dirty_surfaces()?;
        let still_active = self
            .core
            .reconciler
            .surfaces
            .values()
            .any(|s| s.has_active_transitions || s.next_animation_deadline.is_some());
        Ok(had_dirty || still_active)
    }

    fn render_dirty_surfaces(&mut self) -> Result<()> {
        let mut reclaim_candidates = Vec::new();
        for (rk, surf) in &mut self.core.reconciler.surfaces {
            if !surf.needs_redraw {
                continue;
            }
            self.core.assets.ensure_decoded_for_commands(&surf.commands);
            let eff = surf.effective_logical_size();
            let (bw, bh) = eff.to_buffer_ceil(surf.scale);
            let len = (bw.0 as usize) * (bh.0 as usize) * 4;
            let buf = self.frame_buffers.entry(*rk).or_default();
            if buf.len() != len {
                buf.resize(len, 0);
                surf.full_redraw = true;
            }
            let damage = surf.dirty_buffer_rects();
            if cfg!(any(feature = "renderer-software", feature = "renderer-gpu")) {
                self.core.renderer.render_argb8888(
                    buf,
                    bw,
                    bh,
                    &damage,
                    FrameScene {
                        scale: surf.scale,
                        background: surf.spec.background,
                        commands: &surf.commands,
                        assets: &self.core.assets,
                        frame_seq: self.core.reconciler.frame_seq,
                    },
                )?;
            }
            surf.needs_redraw = false;
            surf.full_redraw = false;
            surf.dirty_logical_rects.clear();
            self.core
                .assets
                .collect_reclaimable_from_commands(&surf.commands, &mut reclaim_candidates);
        }
        self.core.assets.reclaim_assets(&reclaim_candidates);
        Ok(())
    }

    fn surface_key_match(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<RetainedSurfaceKey> {
        let skey = surface_key.into_surface_key();
        self.core
            .reconciler
            .surfaces
            .keys()
            .find(|rk| rk.surface_key == skey)
            .copied()
    }

    fn surface_by_key(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<&crate::reconcile::surface::RetainedSurface<A::Message>> {
        let rk = self.surface_key_match(surface_key)?;
        self.core.reconciler.surfaces.get(&rk)
    }

    fn surface_by_key_mut(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<&mut crate::reconcile::surface::RetainedSurface<A::Message>> {
        let rk = self.surface_key_match(surface_key)?;
        self.core.reconciler.surfaces.get_mut(&rk)
    }

    pub fn click_surface_point(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        x: f32,
        y: f32,
    ) -> bool {
        let pos = Point::new(LogicalPx(x), LogicalPx(y));
        let dispatch = self
            .surface_by_key(surface_key)
            .map(|surf| surf.dispatch_click_at(pos));
        dispatch.is_some_and(|d| self.core.dispatch_pointer_result(d, &mut self.inbox))
    }

    /// Simulates a pointer press on a surface point, triggering matching node `on_pointer_down`,
    /// `on_click`, and `on_click_sound` handlers.
    pub fn pointer_down_surface_point(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        x: f32,
        y: f32,
        button: MouseButton,
    ) -> bool {
        let pos = Point::new(LogicalPx(x), LogicalPx(y));
        let dispatch = self
            .surface_by_key(surface_key)
            .map(|surf| surf.dispatch_pointer_down_at(pos, button, Modifiers::empty()));
        dispatch.is_some_and(|d| self.core.dispatch_pointer_result(d, &mut self.inbox))
    }

    /// Simulates pointer motion on a surface point, triggering hover transitions (`on_leave`,
    /// `on_hover`, `on_hover_sound`) and node/surface `on_pointer_move`.
    pub fn pointer_move_surface_point(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        x: f32,
        y: f32,
    ) -> bool {
        let Some(rk) = self.surface_key_match(surface_key) else {
            return false;
        };
        let pos = Point::new(LogicalPx(x), LogicalPx(y));
        self.core
            .dispatch_pointer_move(rk, pos, Modifiers::empty(), true, &mut self.inbox)
            .0
    }

    /// Simulates the pointer leaving a surface, firing `on_leave` for the currently hovered node.
    pub fn pointer_leave_surface(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> bool {
        let Some(surf) = self.surface_by_key_mut(surface_key) else {
            return false;
        };
        if let Some(tr) = surf.update_hover(None)
            && let Some(msg) = tr.on_leave
        {
            self.inbox.push_back(msg);
            return true;
        }
        false
    }

    /// Simulates a pointer release on a surface point, triggering node/surface `on_pointer_up`.
    pub fn pointer_up_surface_point(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        x: f32,
        y: f32,
        button: MouseButton,
    ) -> bool {
        let pos = Point::new(LogicalPx(x), LogicalPx(y));
        let dispatch = self
            .surface_by_key(surface_key)
            .map(|surf| surf.dispatch_pointer_up_at(pos, button, Modifiers::empty()));
        dispatch.is_some_and(|d| self.core.dispatch_pointer_result(d, &mut self.inbox))
    }

    /// Simulates a pointer scroll/wheel event on a surface point with `(delta_x, delta_y)` and
    /// `(discrete_x, discrete_y)`, triggering the topmost matching `on_scroll` handler.
    pub fn scroll_surface_point(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        x: f32,
        y: f32,
        delta: (f32, f32),
        discrete: (i32, i32),
    ) -> bool {
        self.scroll_surface_event(
            surface_key,
            ScrollEvent {
                position: Point::new(LogicalPx(x), LogicalPx(y)),
                local_position: Point::ZERO,
                delta_x: delta.0,
                delta_y: delta.1,
                discrete_x: discrete.0,
                discrete_y: discrete.1,
                source: ScrollSource::Unknown,
                stop_x: false,
                stop_y: false,
                modifiers: Modifiers::empty(),
            },
        )
    }

    /// Simulates a full [`ScrollEvent`] (including [`ScrollSource`], `stop_x`/`stop_y`, and
    /// modifiers) on a surface.
    pub fn scroll_surface_event(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        event: ScrollEvent,
    ) -> bool {
        let msg = self
            .surface_by_key(surface_key)
            .and_then(|surf| surf.dispatch_scroll_at(event));
        if let Some(m) = msg {
            self.inbox.push_back(m);
            true
        } else {
            false
        }
    }

    /// Simulates a multi-touch [`TouchEvent`] on a surface (routing to `on_touch` handlers with
    /// per-contact capture, and falling back unclaimed primary contacts to pointer events).
    pub fn touch_surface(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        event: TouchEvent,
    ) -> bool {
        let Some(rk) = self.surface_key_match(surface_key) else {
            return false;
        };
        self.core.dispatch_touch(rk, event, &mut self.inbox)
    }

    /// Simulates a touchpad multi-finger [`GestureEvent`] on a surface.
    pub fn gesture_surface(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        event: GestureEvent,
    ) -> bool {
        let Some(rk) = self.surface_key_match(surface_key) else {
            return false;
        };
        self.core.dispatch_gesture(rk, event, &mut self.inbox)
    }

    /// Simulates a pen tablet / stylus / pad [`TabletEvent`] on a surface (routing to `on_tablet`
    /// handlers with per-tool capture, and falling back unclaimed stylus actions to pointer
    /// events).
    pub fn tablet_surface(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        event: TabletEvent,
    ) -> bool {
        let Some(rk) = self.surface_key_match(surface_key) else {
            return false;
        };
        self.core.dispatch_tablet(rk, event, &mut self.inbox)
    }

    /// Simulates a keyboard key event targeting the surface's `on_key` handler.
    pub fn key_surface(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        event: KeyEvent,
    ) -> bool {
        let msg = self
            .surface_by_key(surface_key)
            .and_then(|surf| surf.on_key.as_ref())
            .and_then(|on_key| on_key.call(event));
        if let Some(m) = msg {
            self.inbox.push_back(m);
            true
        } else {
            false
        }
    }

    /// Simulates a Wayland IME event (`Enter`, `Leave`, `Update`) targeting the surface's `on_ime`
    /// handler.
    pub fn simulate_ime(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        event: ImeEvent,
    ) -> bool {
        let msg = self
            .surface_by_key(surface_key)
            .and_then(|surf| surf.on_ime.as_ref())
            .and_then(|on_ime| on_ime.call(event));
        if let Some(m) = msg {
            self.inbox.push_back(m);
            true
        } else {
            false
        }
    }

    /// Returns the currently reconciled [`ImeConfig`] for a retained surface, if present.
    #[must_use]
    pub fn surface_ime_config(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<&ImeConfig> {
        self.surface_by_key(surface_key).map(|surf| &surf.spec.ime)
    }

    /// Resolves the effective [`CursorShape`] at surface-local `(x, y)`.
    #[must_use]
    pub fn surface_cursor_at(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        x: f32,
        y: f32,
    ) -> Option<CursorShape> {
        self.surface_by_key(surface_key)
            .map(|surf| surf.cursor_at(Point::new(LogicalPx(x), LogicalPx(y))))
    }

    /// Returns the reconciled [`WindowDecoration`] mode for a retained surface.
    #[must_use]
    pub fn surface_decoration(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<WindowDecoration> {
        self.surface_by_key(surface_key)
            .map(|surf| surf.spec.decoration)
    }

    /// Returns the reconciled [`BackgroundBlur`] configuration for a retained surface.
    #[must_use]
    pub fn surface_background_blur(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<BackgroundBlur> {
        self.surface_by_key(surface_key)
            .map(|surf| surf.spec.background_blur)
    }

    /// Returns the reconciled [`WindowIcon`] for a retained surface, if set.
    #[must_use]
    pub fn surface_window_icon(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<&WindowIcon> {
        self.surface_by_key(surface_key)
            .and_then(|surf| surf.spec.window_icon.as_ref())
    }

    /// Returns the reconciled [`ColorSpace`] for a retained surface.
    #[must_use]
    pub fn surface_color_space(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<ColorSpace> {
        self.surface_by_key(surface_key)
            .map(|surf| surf.spec.color_space)
    }

    /// Returns all `zwlr_gamma_control_manager_v1` actions recorded so far.
    #[must_use]
    pub fn recorded_gamma_actions(&self) -> &[HeadlessGammaAction] {
        &self.recorded_gamma_actions
    }

    /// Returns whether a retained surface currently requests idle inhibition.
    #[must_use]
    pub fn surface_inhibits_idle(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<bool> {
        self.surface_by_key(surface_key)
            .map(|surf| surf.spec.inhibit_idle)
    }

    /// Returns the reconciled [`PointerConstraintMode`] for a retained surface.
    #[must_use]
    pub fn surface_pointer_constraint(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<PointerConstraintMode> {
        self.surface_by_key(surface_key)
            .map(|surf| surf.spec.pointer_constraint)
    }

    /// Simulates a `zwp_relative_pointer_v1` event targeting the surface's `on_relative_pointer`.
    pub fn simulate_relative_pointer(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        event: RelativePointerEvent,
    ) -> bool {
        let msg = self
            .surface_by_key(surface_key)
            .and_then(|surf| surf.on_relative_pointer.as_ref())
            .and_then(|h| h.call(event));
        if let Some(m) = msg {
            self.inbox.push_back(m);
            true
        } else {
            false
        }
    }

    /// Simulates a `wl_data_device` Drag & Drop event targeting the surface's `on_dnd`.
    pub fn simulate_dnd(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        event: DndEvent,
    ) -> bool {
        let msg = self
            .surface_by_key(surface_key)
            .and_then(|surf| surf.on_dnd.as_ref())
            .and_then(|h| h.call(event));
        if let Some(m) = msg {
            self.inbox.push_back(m);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        &self.core.capabilities
    }

    #[must_use]
    pub fn memory_config(&self) -> &MemoryConfig {
        &self.core.memory
    }

    #[must_use]
    pub fn state(&self) -> &A::State {
        &self.state
    }

    #[must_use]
    pub fn quit_requested(&self) -> bool {
        self.core.quit_requested
    }

    #[must_use]
    pub fn surface_count(&self) -> usize {
        self.core.reconciler.surfaces.len()
    }

    #[must_use]
    pub fn surface_logical_size(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<LogicalSize> {
        self.surface_by_key(surface_key)
            .map(|s| s.effective_logical_size())
    }

    /// Simulates a compositor `xdg_popup.popup_done` dismissal on a popup surface, firing its
    /// `on_close` message without requesting application exit.
    pub fn simulate_popup_dismiss(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> bool {
        let Some(surf) = self.surface_by_key(surface_key) else {
            return false;
        };
        if surf.spec.kind != crate::surface::SurfaceKind::Popup {
            return false;
        }
        if let Some(msg) = surf.on_close.clone() {
            self.inbox.push_back(msg);
            true
        } else {
            false
        }
    }

    /// Returns all `(SurfaceKey, token)` pairs recorded via
    /// [`EffectCtx::activate_surface`](crate::effect::EffectCtx::activate_surface) when
    /// [`Capability::XdgActivation`] is enabled.
    #[must_use]
    pub fn activated_surfaces(&self) -> &[(SurfaceKey, String)] {
        &self.activated_surfaces
    }

    /// Injects a synthetic [`ScreencopyFrame`] returned by
    /// [`EffectCtx::capture_screen`](crate::effect::EffectCtx::capture_screen) when
    /// [`Capability::Screencopy`] is enabled. Passing `None` causes headless screencopy requests
    /// to return `Err(WaywiError::InvalidState(..))`.
    pub fn set_synthetic_screencopy(&mut self, frame: Option<ScreencopyFrame>) {
        self.synthetic_screencopy = frame;
    }

    /// Returns all foreign-toplevel actions recorded via [`EffectCtx`](crate::effect::EffectCtx)
    /// when [`Capability::ForeignToplevelControl`] is enabled.
    #[must_use]
    pub fn recorded_toplevel_actions(&self) -> &[HeadlessToplevelAction] {
        &self.recorded_toplevel_actions
    }

    /// Returns all `ext_workspace_v1` actions recorded via [`EffectCtx`](crate::effect::EffectCtx)
    /// when [`Capability::ExtWorkspace`] is enabled.
    #[must_use]
    pub fn recorded_ext_workspace_actions(&self) -> &[HeadlessExtWorkspaceAction] {
        &self.recorded_ext_workspace_actions
    }

    /// Returns all virtual-keyboard actions recorded via [`EffectCtx`](crate::effect::EffectCtx)
    /// when [`Capability::VirtualKeyboard`] is enabled.
    #[must_use]
    pub fn recorded_virtual_keyboard_actions(&self) -> &[HeadlessVirtualKeyboardAction] {
        &self.recorded_virtual_keyboard_actions
    }

    /// Returns all outgoing Drag & Drop requests recorded via
    /// [`EffectCtx::start_drag`](crate::effect::EffectCtx::start_drag) when
    /// [`Capability::DragAndDrop`] is enabled.
    #[must_use]
    pub fn recorded_drag_starts(&self) -> &[HeadlessDragStart] {
        &self.recorded_drag_starts
    }

    /// Simulates a compositor `configure` event updating `configured_sizes` for a surface.
    pub fn simulate_surface_configure(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        size: LogicalSize,
    ) -> bool {
        let skey = surface_key.into_surface_key();
        let keys: Vec<RetainedSurfaceKey> = self
            .core
            .reconciler
            .surfaces
            .keys()
            .filter(|rk| rk.surface_key == skey)
            .copied()
            .collect();
        if keys.is_empty() {
            return false;
        }
        let mut changed = false;
        for rk in keys {
            if self.core.configured_sizes.insert(rk, size) != Some(size) {
                changed = true;
            }
        }
        if changed {
            self.core.memo_store.invalidate_surface(skey.0);
        }
        true
    }

    #[must_use]
    pub fn arena_heap_fallbacks(&self) -> usize {
        self.core.arena.last_stats().heap_fallbacks
    }

    /// Returns `true` if `asset`'s decoded RGBA8 pixel buffer is currently resident in RAM.
    #[must_use]
    pub fn asset_pixels_resident(&self, asset: crate::image::ImageAsset) -> bool {
        self.core.assets.is_decoded(asset.id)
    }

    /// Returns total decoded RGBA8 pixel bytes currently resident in the internal asset cache.
    #[must_use]
    pub fn decoded_asset_bytes(&self) -> usize {
        self.core.assets.decoded_pixel_bytes()
    }

    #[must_use]
    pub fn rendered_buffer(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<&[u8]> {
        let skey = surface_key.into_surface_key();
        self.frame_buffers
            .iter()
            .find(|(rk, _)| rk.surface_key == skey)
            .map(|(_, b)| b.as_slice())
    }

    /// Builds a full AccessKit / AT-SPI2 tree snapshot for the current retained surfaces.
    #[cfg(feature = "accessibility")]
    #[must_use]
    pub fn accessibility_tree(&mut self) -> crate::accessibility::AccessibleTreeUpdate {
        crate::accessibility::build_tree_update(
            &mut self.a11y_tree_state,
            &self.core.reconciler.surfaces,
            None,
            "waywi-headless",
        )
    }

    /// Dispatches a simulated AT-SPI2 / AccessKit action request against retained surfaces.
    #[cfg(feature = "accessibility")]
    pub fn dispatch_accessibility_action(
        &mut self,
        request: crate::accessibility::AccessibleActionRequest,
    ) -> bool {
        crate::accessibility::apply_accessibility_action(
            &mut self.a11y_tree_state,
            &self.core.reconciler.surfaces,
            request.action,
            request.target_node,
            &mut self.inbox,
        )
    }
}
