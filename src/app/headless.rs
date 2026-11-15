use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crate::arena::FrameArenaPool;
use crate::asset::AssetCache;
use crate::capabilities::Capabilities;
use crate::display::OutputInfo;
use crate::effect::{AsyncAssetWorker, EffectCtx};
use crate::error::Result;
use crate::geometry::{LogicalPx, LogicalSize, Point, ScaleFactor};
use crate::input::{Modifiers, MouseButton};
use crate::reconcile::{PointerDispatch, ReconcileTargets, Reconciler, RetainedSurfaceKey};
use crate::render::{
    ActiveRenderer, MemoStore, RenderBackend, RendererPreference, enter_memo_scope,
};
use crate::system::SystemProviders;
use crate::timer::TimerScheduler;

use super::runner::Application;
use super::subscription_mgr::flatten_specs;

/// Deterministic headless runtime driving any [`Application`] without opening a Wayland socket.
///
/// Designed for unit/integration tests and Criterion event-dispatch benchmarks verifying view
/// reconciliation, memo skipping, zero steady-state arena leaks, hit-testing, timers, and
/// rendering pipelines cleanly.
pub struct HeadlessRuntime<A: Application> {
    state: A::State,
    capabilities: Capabilities,
    outputs: Vec<OutputInfo>,
    assets: AssetCache,
    asset_worker: AsyncAssetWorker,
    system: SystemProviders,
    renderer: ActiveRenderer,
    arena: FrameArenaPool,
    memo_store: MemoStore,
    reconciler: Reconciler<A::Message>,
    timers: TimerScheduler<A::Message>,
    configured_sizes: HashMap<RetainedSurfaceKey, LogicalSize>,
    surface_scales: HashMap<RetainedSurfaceKey, ScaleFactor>,
    inbox: VecDeque<A::Message>,
    quit_requested: bool,
    frame_buffers: HashMap<RetainedSurfaceKey, Vec<u8>>,
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    audio: crate::audio::AudioClient,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    player: crate::audio::AudioPlayer,
    #[cfg(feature = "hyprland-ipc")]
    hyprland: Option<crate::hyprland::HyprlandClient>,
    #[cfg(feature = "sway-ipc")]
    sway: Option<crate::sway::SwayClient>,
    #[cfg(feature = "dbus")]
    dbus: crate::dbus::DbusClient,
    #[cfg(feature = "accessibility")]
    a11y_tree_state: crate::accessibility::AccessibilityTreeState,
}

impl<A: Application> HeadlessRuntime<A> {
    /// Boots `A` against a synthetic output list and explicit [`Capabilities`].
    pub fn new(capabilities: Capabilities, outputs: Vec<OutputInfo>) -> Result<Self> {
        Self::new_with_renderer(capabilities, outputs, RendererPreference::Auto)
    }

    /// Boots `A` against synthetic outputs using an explicit [`RendererPreference`].
    pub fn new_with_renderer(
        mut capabilities: Capabilities,
        outputs: Vec<OutputInfo>,
        renderer_pref: RendererPreference,
    ) -> Result<Self> {
        let renderer = ActiveRenderer::from_preference(renderer_pref, None);
        capabilities.set(
            crate::capabilities::Capability::GpuRenderer,
            renderer.is_gpu_active(),
        );
        let mut assets = AssetCache::new();
        let asset_worker = AsyncAssetWorker::new();
        let mut system = SystemProviders::new();
        let mut quit_requested = false;
        let mut pending_after = Vec::new();
        let mut reloaded_assets = Vec::new();

        #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
        let audio = crate::audio::AudioClient::new();
        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        let player = crate::audio::AudioPlayer::new();
        #[cfg(feature = "hyprland-ipc")]
        let hyprland = None;
        #[cfg(feature = "sway-ipc")]
        let sway = None;
        #[cfg(feature = "dbus")]
        let dbus = crate::dbus::DbusClient::new();

        let state = {
            let mut ctx = EffectCtx {
                quit_requested: &mut quit_requested,
                capabilities: &capabilities,
                outputs: &outputs,
                assets: &mut assets,
                asset_worker: &asset_worker,
                system: &mut system,
                pending_after: &mut pending_after,
                reloaded_assets: &mut reloaded_assets,
                #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
                audio: &audio,
                #[cfg(any(
                    feature = "pipewire",
                    feature = "pulseaudio",
                    feature = "alsa",
                    feature = "audio-playback"
                ))]
                player: &player,
                #[cfg(feature = "hyprland-ipc")]
                hyprland: hyprland.as_ref(),
                #[cfg(feature = "sway-ipc")]
                sway: sway.as_ref(),
                #[cfg(feature = "dbus")]
                dbus: &dbus,
                #[cfg(feature = "clipboard")]
                clipboard_writer: None,
            };
            A::init(&mut ctx)?
        };

        let mut timers = TimerScheduler::new();
        let now = Instant::now();
        for (delay, factory) in pending_after {
            if let Ok(msg) = factory().downcast::<A::Message>() {
                timers.push_imperative(now + delay, *msg);
            }
        }

        let mut rt = Self {
            state,
            capabilities,
            outputs,
            assets,
            asset_worker,
            system,
            renderer,
            arena: FrameArenaPool::new(),
            memo_store: MemoStore::default(),
            reconciler: Reconciler::new(),
            timers,
            configured_sizes: HashMap::new(),
            surface_scales: HashMap::new(),
            inbox: VecDeque::new(),
            quit_requested,
            frame_buffers: HashMap::new(),
            #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
            audio,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            player,
            #[cfg(feature = "hyprland-ipc")]
            hyprland,
            #[cfg(feature = "sway-ipc")]
            sway,
            #[cfg(feature = "dbus")]
            dbus,
            #[cfg(feature = "accessibility")]
            a11y_tree_state: crate::accessibility::AccessibilityTreeState::default(),
        };
        rt.step_at(now)?;
        Ok(rt)
    }

    pub fn enqueue(&mut self, msg: A::Message) {
        self.inbox.push_back(msg);
    }

    pub fn step(&mut self) -> Result<()> {
        self.step_at(Instant::now())
    }

    pub fn step_at(&mut self, now: Instant) -> Result<()> {
        for msg in self.timers.collect_due(now) {
            self.inbox.push_back(msg);
        }
        for msg in self.system.collect_due::<A::Message>(now) {
            self.inbox.push_back(msg);
        }
        let (_, _) = self.reconciler.tick_animations(now);

        while let Some(msg) = self.inbox.pop_front() {
            let mut pending_after = Vec::new();
            let mut reloaded_assets = Vec::new();
            let mut ctx = EffectCtx {
                quit_requested: &mut self.quit_requested,
                capabilities: &self.capabilities,
                outputs: &self.outputs,
                assets: &mut self.assets,
                asset_worker: &self.asset_worker,
                system: &mut self.system,
                pending_after: &mut pending_after,
                reloaded_assets: &mut reloaded_assets,
                #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
                audio: &self.audio,
                #[cfg(any(
                    feature = "pipewire",
                    feature = "pulseaudio",
                    feature = "alsa",
                    feature = "audio-playback"
                ))]
                player: &self.player,
                #[cfg(feature = "hyprland-ipc")]
                hyprland: self.hyprland.as_ref(),
                #[cfg(feature = "sway-ipc")]
                sway: self.sway.as_ref(),
                #[cfg(feature = "dbus")]
                dbus: &self.dbus,
                #[cfg(feature = "clipboard")]
                clipboard_writer: None,
            };
            A::update(&mut self.state, msg, &mut ctx)?;
            for (delay, factory) in pending_after {
                if let Ok(m) = factory().downcast::<A::Message>() {
                    self.timers.push_imperative(now + delay, *m);
                }
            }
            for (aid, rev) in reloaded_assets {
                self.reconciler.invalidate_asset(aid, rev);
            }
        }

        let _arena_guard = self.arena.enter_scope();
        let subs = A::subscriptions(&self.state);
        let mut flat = Vec::new();
        flatten_specs(&subs, &mut flat);
        self.timers.reconcile(flat.iter().copied(), now);
        self.system.reconcile(flat.iter().copied());

        let report = {
            let _memo_guard = enter_memo_scope(&mut self.memo_store);
            let declared = A::view(&self.state);
            self.reconciler.reconcile(
                declared,
                &ReconcileTargets {
                    outputs: &self.outputs,
                    configured_sizes: &self.configured_sizes,
                    surface_scales: &self.surface_scales,
                },
                &mut self.renderer,
                &self.assets,
                now,
            )
        };
        self.memo_store.prune_stale();

        for rk in &report.removed_surfaces {
            self.frame_buffers.remove(rk);
        }

        for (rk, surf) in &mut self.reconciler.surfaces {
            if !surf.needs_redraw {
                continue;
            }
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
                self.renderer.render_argb8888(
                    buf,
                    bw,
                    bh,
                    &damage,
                    crate::render::FrameScene {
                        scale: surf.scale,
                        background: surf.spec.background,
                        commands: &surf.commands,
                        assets: &self.assets,
                        frame_seq: self.reconciler.frame_seq,
                    },
                )?;
            }
            surf.needs_redraw = false;
            surf.full_redraw = false;
            surf.dirty_logical_rects.clear();
        }

        Ok(())
    }

    fn enqueue_pointer_dispatch(&mut self, dispatch: PointerDispatch<A::Message>) -> bool {
        let mut dispatched = false;
        if let Some(msg) = dispatch.on_click {
            self.inbox.push_back(msg);
            dispatched = true;
        }
        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        if let Some((sound, vol)) = dispatch.click_sound {
            self.player.play_with_volume(sound, vol);
            dispatched = true;
        }
        if let Some(msg) = dispatch.on_pointer {
            self.inbox.push_back(msg);
            dispatched = true;
        }
        if let Some(msg) = dispatch.on_surface {
            self.inbox.push_back(msg);
            dispatched = true;
        }
        dispatched
    }

    fn surface_by_key(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<&crate::reconcile::surface::RetainedSurface<A::Message>> {
        let skey = surface_key.into_surface_key();
        self.reconciler
            .surfaces
            .iter()
            .find(|(rk, _)| rk.surface_key == skey)
            .map(|(_, s)| s)
    }

    fn surface_by_key_mut(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<&mut crate::reconcile::surface::RetainedSurface<A::Message>> {
        let skey = surface_key.into_surface_key();
        self.reconciler
            .surfaces
            .iter_mut()
            .find(|(rk, _)| rk.surface_key == skey)
            .map(|(_, s)| s)
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
        dispatch.is_some_and(|d| self.enqueue_pointer_dispatch(d))
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
        dispatch.is_some_and(|d| self.enqueue_pointer_dispatch(d))
    }

    /// Simulates pointer motion on a surface point, triggering hover transitions (`on_leave`,
    /// `on_hover`, `on_hover_sound`) and node/surface `on_pointer_move`.
    pub fn pointer_move_surface_point(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        x: f32,
        y: f32,
    ) -> bool {
        let pos = Point::new(LogicalPx(x), LogicalPx(y));
        let Some(surf) = self.surface_by_key_mut(surface_key) else {
            return false;
        };
        let move_dispatch = surf.dispatch_pointer_move_at(pos, Modifiers::empty(), true);
        let mut dispatched = false;
        if let Some(tr) = move_dispatch.hover {
            if let Some(msg) = tr.on_leave {
                self.inbox.push_back(msg);
                dispatched = true;
            }
            if let Some(msg) = tr.on_hover {
                self.inbox.push_back(msg);
                dispatched = true;
            }
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            if let Some((sound, vol)) = tr.on_hover_sound {
                self.player.play_with_volume(sound, vol);
                dispatched = true;
            }
        }
        if let Some(msg) = move_dispatch.on_node_move {
            self.inbox.push_back(msg);
            dispatched = true;
        }
        if let Some(msg) = move_dispatch.on_surface_move {
            self.inbox.push_back(msg);
            dispatched = true;
        }
        dispatched
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
        dispatch.is_some_and(|d| self.enqueue_pointer_dispatch(d))
    }

    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    #[must_use]
    pub fn state(&self) -> &A::State {
        &self.state
    }

    #[must_use]
    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    #[must_use]
    pub fn surface_count(&self) -> usize {
        self.reconciler.surfaces.len()
    }

    #[must_use]
    pub fn surface_logical_size(
        &self,
        surface_key: impl crate::surface::IntoSurfaceKey,
    ) -> Option<LogicalSize> {
        self.surface_by_key(surface_key)
            .map(|s| s.effective_logical_size())
    }

    /// Simulates a compositor `configure` event updating `configured_sizes` for a surface.
    pub fn simulate_surface_configure(
        &mut self,
        surface_key: impl crate::surface::IntoSurfaceKey,
        size: LogicalSize,
    ) -> bool {
        let skey = surface_key.into_surface_key();
        let keys: Vec<RetainedSurfaceKey> = self
            .reconciler
            .surfaces
            .keys()
            .filter(|rk| rk.surface_key == skey)
            .copied()
            .collect();
        if keys.is_empty() {
            return false;
        }
        for rk in keys {
            self.configured_sizes.insert(rk, size);
        }
        true
    }

    #[must_use]
    pub fn arena_heap_fallbacks(&self) -> usize {
        self.arena.last_stats().heap_fallbacks
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
            &self.reconciler.surfaces,
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
            &self.reconciler.surfaces,
            request.action,
            request.target_node,
            &mut self.inbox,
        )
    }
}
