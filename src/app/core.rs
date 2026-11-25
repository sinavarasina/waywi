use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use wayland_client::Connection;

use crate::arena::FrameArenaPool;
use crate::asset::{AssetCache, AssetId};
use crate::capabilities::{Capabilities, Capability};
use crate::display::OutputInfo;
#[cfg(feature = "clipboard")]
use crate::effect::context::ClipboardSetter;
use crate::effect::{ActivationCallback, AssetCallback, AsyncAssetWorker, EffectCtx};
use crate::error::Result;
use crate::geometry::{LogicalSize, Point, ScaleFactor};
use crate::input::Modifiers;
use crate::reconcile::surface::ReconcileReport;
use crate::reconcile::{PointerDispatch, ReconcileTargets, Reconciler, RetainedSurfaceKey};
use crate::render::{
    ActiveRenderer, MemoStore, RenderBackend, RendererPreference, enter_memo_scope,
};
use crate::surface::{SurfaceKey, Surfaces};
use crate::system::{SystemProviders, SystemSamplers};
use crate::text::SharedStr;
use crate::timer::TimerScheduler;

use super::memory::MemoryConfig;

/// Post-effect follow-up actions handled by the outer Wayland or headless runtime.
pub(crate) struct EffectPostWork<M> {
    pub(crate) activation_requests: Vec<(Option<String>, ActivationCallback<M>)>,
    pub(crate) surface_activations: Vec<(SurfaceKey, String)>,
    pub(crate) trim_requested: bool,
}

/// Shared runtime engine owning all Wayland-independent subsystems used by both
/// [`LoopRuntime`](super::event_loop::LoopRuntime) and [`HeadlessRuntime`](super::HeadlessRuntime).
pub(crate) struct CoreEngine<M: Clone + 'static> {
    pub(crate) capabilities: Capabilities,
    pub(crate) memory: MemoryConfig,
    pub(crate) app_name: SharedStr,
    pub(crate) assets: AssetCache,
    pub(crate) asset_worker: AsyncAssetWorker,
    pub(crate) next_asset_job_id: u64,
    pub(crate) pending_asset_callbacks: HashMap<u64, AssetCallback<M>>,
    pub(crate) system: SystemProviders,
    pub(crate) system_samplers: SystemSamplers<M>,
    pub(crate) renderer: ActiveRenderer,
    pub(crate) arena: FrameArenaPool,
    pub(crate) memo_store: MemoStore,
    pub(crate) reconciler: Reconciler<M>,
    pub(crate) timers: TimerScheduler<M>,
    pub(crate) configured_sizes: HashMap<RetainedSurfaceKey, LogicalSize>,
    pub(crate) surface_scales: HashMap<RetainedSurfaceKey, ScaleFactor>,
    pub(crate) quit_requested: bool,

    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    pub(crate) audio_client: crate::audio::AudioClient,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub(crate) player: crate::audio::AudioPlayer,
    #[cfg(feature = "hyprland-ipc")]
    pub(crate) hyprland_client: Option<crate::compositor::hyprland::HyprlandClient>,
    #[cfg(feature = "sway-ipc")]
    pub(crate) sway_client: Option<crate::compositor::sway::SwayClient>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_client: crate::dbus::DbusClient,
}

impl<M: Clone + 'static> CoreEngine<M> {
    pub(crate) fn new(
        mut capabilities: Capabilities,
        renderer_pref: RendererPreference,
        wayland_conn: Option<&Connection>,
        memory: MemoryConfig,
        app_name: SharedStr,
        probe_session_ipc: bool,
    ) -> Result<Self> {
        let renderer = ActiveRenderer::from_preference(renderer_pref, wayland_conn, &memory)?;
        capabilities.set(Capability::GpuRenderer, renderer.is_gpu_active());

        #[cfg(feature = "hyprland-ipc")]
        let hyprland_client = if probe_session_ipc {
            let client = crate::compositor::hyprland::HyprlandClient::from_env().ok();
            capabilities.set(Capability::HyprlandIpc, client.is_some());
            client
        } else {
            None
        };

        #[cfg(feature = "sway-ipc")]
        let sway_client = if probe_session_ipc {
            let client = crate::compositor::sway::SwayClient::from_env().ok();
            capabilities.set(Capability::SwayIpc, client.is_some());
            client
        } else {
            None
        };

        #[cfg(feature = "dbus")]
        let dbus_client = crate::dbus::DbusClient::with_config(
            memory.dbus_cache_snapshots,
            memory.dbus_idle_disconnect,
        );

        #[cfg(feature = "global-shortcuts")]
        if probe_session_ipc {
            capabilities.set(
                Capability::GlobalShortcuts,
                capabilities.has(Capability::Dbus),
            );
        }

        let _ = probe_session_ipc;

        Ok(Self {
            capabilities,
            assets: AssetCache::with_default_retention(memory.default_asset_retention),
            asset_worker: AsyncAssetWorker::new(),
            next_asset_job_id: 1,
            pending_asset_callbacks: HashMap::new(),
            system: SystemProviders::new(),
            system_samplers: SystemSamplers::new(),
            renderer,
            arena: FrameArenaPool::with_limits(
                memory.arena_initial_bytes,
                memory.arena_max_retain_bytes,
            ),
            memo_store: MemoStore::default(),
            reconciler: Reconciler::new(),
            timers: TimerScheduler::new(),
            configured_sizes: HashMap::new(),
            surface_scales: HashMap::new(),
            quit_requested: false,
            #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
            audio_client: crate::audio::AudioClient::new(),
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            player: crate::audio::AudioPlayer::with_cache_limit(memory.sound_cache_max_bytes),
            #[cfg(feature = "hyprland-ipc")]
            hyprland_client,
            #[cfg(feature = "sway-ipc")]
            sway_client,
            #[cfg(feature = "dbus")]
            dbus_client,
            memory,
            app_name,
        })
    }

    pub(crate) fn run_effect<R>(
        &mut self,
        outputs: &[OutputInfo],
        #[cfg(feature = "clipboard")] clipboard_writer: Option<ClipboardSetter<'_>>,
        now: Instant,
        f: impl FnOnce(&mut EffectCtx<'_, M>) -> R,
    ) -> (R, EffectPostWork<M>) {
        let mut trim_requested = false;
        let mut pending_after: Vec<(Duration, M)> = Vec::new();
        let mut pending_asset_jobs = Vec::new();
        let mut reloaded_assets: Vec<(AssetId, u64)> = Vec::new();
        let mut evicted_assets: Vec<AssetId> = Vec::new();
        let mut activation_requests = Vec::new();
        let mut surface_activations = Vec::new();

        #[cfg(feature = "clipboard")]
        let has_clipboard = clipboard_writer.is_some();
        #[cfg(feature = "clipboard")]
        let mut clip_holder = clipboard_writer;
        #[cfg(feature = "clipboard")]
        let mut clip_forward = |sel: crate::clipboard::ClipboardSelection,
                                entries: Vec<(String, Vec<u8>)>| {
            clip_holder
                .as_deref_mut()
                .expect("clipboard_writer present")(sel, entries)
        };

        #[cfg(feature = "text")]
        let frame_seq = self.reconciler.frame_seq;
        #[cfg(feature = "text")]
        let mut measure_cursor =
            |content: &SharedStr, style: crate::text::TextMeasureStyle<'_>, cursor_byte: usize| {
                self.renderer
                    .measure_text_cursor(content, style, cursor_byte, frame_seq)
            };

        let result = {
            let mut ctx = EffectCtx {
                quit_requested: &mut self.quit_requested,
                trim_requested: &mut trim_requested,
                capabilities: &self.capabilities,
                outputs,
                memory_config: &self.memory,
                assets: &mut self.assets,
                pending_asset_jobs: &mut pending_asset_jobs,
                system: &mut self.system,
                pending_after: &mut pending_after,
                reloaded_assets: &mut reloaded_assets,
                evicted_assets: &mut evicted_assets,
                pending_activation_requests: &mut activation_requests,
                pending_surface_activations: &mut surface_activations,
                #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
                audio: &self.audio_client,
                #[cfg(any(
                    feature = "pipewire",
                    feature = "pulseaudio",
                    feature = "alsa",
                    feature = "audio-playback"
                ))]
                player: &self.player,
                #[cfg(feature = "hyprland-ipc")]
                hyprland: self.hyprland_client.as_ref(),
                #[cfg(feature = "sway-ipc")]
                sway: self.sway_client.as_ref(),
                #[cfg(feature = "dbus")]
                dbus: &self.dbus_client,
                #[cfg(feature = "clipboard")]
                clipboard_writer: if has_clipboard {
                    Some(&mut clip_forward)
                } else {
                    None
                },
                #[cfg(feature = "text")]
                text_cursor_measurer: &mut measure_cursor,
            };
            f(&mut ctx)
        };

        for (delay, msg) in pending_after {
            self.timers.push_imperative(now + delay, msg);
        }
        for (path, cb) in pending_asset_jobs {
            let job_id = self.next_asset_job_id;
            self.next_asset_job_id = self.next_asset_job_id.wrapping_add(1);
            self.pending_asset_callbacks.insert(job_id, cb);
            self.asset_worker.submit(job_id, path);
        }
        for (aid, rev) in reloaded_assets {
            self.reconciler.invalidate_asset(aid, rev);
        }
        for aid in evicted_assets {
            self.renderer.evict_asset(aid);
        }
        if trim_requested {
            self.trim_memory();
        }

        (
            result,
            EffectPostWork {
                activation_requests,
                surface_activations,
                trim_requested,
            },
        )
    }

    pub(crate) fn trim_memory(&mut self) {
        self.assets.trim_reclaimable();
        self.renderer.trim_memory();
        self.arena.trim();
        self.memo_store.trim();
        self.system.trim_memory();
        self.system_samplers.trim();
        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        self.player.trim_memory();
        #[cfg(feature = "dbus")]
        self.dbus_client.trim_cache();
    }

    /// Collects due timers, system sampler ticks, completed async asset loads, and sprite
    /// animation frames, returning surface keys dirtied by animations.
    pub(crate) fn collect_core_due(
        &mut self,
        now: Instant,
        out_msgs: &mut VecDeque<M>,
    ) -> Vec<RetainedSurfaceKey> {
        out_msgs.extend(self.timers.collect_due(now));
        out_msgs.extend(self.system_samplers.collect_due(now, &mut self.system));

        for done in self.asset_worker.drain_completed() {
            let asset_res = done.decoded.map(|dec| {
                self.assets.commit_worker_decoded(
                    Some(done.path),
                    done.format,
                    done.content_hash,
                    dec,
                )
            });
            if let Some(cb) = self.pending_asset_callbacks.remove(&done.job_id) {
                out_msgs.push_back(cb(asset_res));
            }
        }

        let (anim_dirty, _, anim_msgs) = self.reconciler.tick_animations(now);
        out_msgs.extend(anim_msgs);
        anim_dirty
    }

    #[must_use]
    pub(crate) fn next_core_deadline(&self, now: Instant) -> Option<Instant> {
        let next_timer = self.timers.next_deadline();
        let next_sys = self.system_samplers.next_deadline(now);
        let next_anim = self.reconciler.next_animation_deadline();
        [next_timer, next_sys, next_anim]
            .into_iter()
            .flatten()
            .min()
    }

    pub(crate) fn reconcile_view(
        &mut self,
        outputs: &[OutputInfo],
        now: Instant,
        build_view: impl FnOnce() -> Surfaces<M>,
    ) -> ReconcileReport {
        let targets = ReconcileTargets {
            outputs,
            configured_sizes: &self.configured_sizes,
            surface_scales: &self.surface_scales,
            default_buffer_retention: self.memory.default_buffer_retention,
            app_name: &self.app_name,
        };
        let report = {
            let _memo_guard = enter_memo_scope(&mut self.memo_store);
            let declared = build_view();
            self.reconciler
                .reconcile(declared, &targets, &mut self.renderer, &self.assets, now)
        };
        self.memo_store.prune_stale();
        report
    }

    pub(crate) fn dispatch_pointer_result(
        &self,
        d: PointerDispatch<M>,
        out_msgs: &mut VecDeque<M>,
    ) -> bool {
        let mut dispatched = false;
        if let Some(msg) = d.on_click {
            out_msgs.push_back(msg);
            dispatched = true;
        }
        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        if let Some(sound) = d.click_sound {
            self.player.play_trigger(sound);
            dispatched = true;
        }
        if let Some(msg) = d.on_pointer {
            out_msgs.push_back(msg);
            dispatched = true;
        }
        if let Some(msg) = d.on_surface {
            out_msgs.push_back(msg);
            dispatched = true;
        }
        dispatched
    }

    pub(crate) fn dispatch_pointer_move(
        &mut self,
        key: RetainedSurfaceKey,
        pos: Point,
        modifiers: Modifiers,
        emit_move: bool,
        out_msgs: &mut VecDeque<M>,
    ) -> bool {
        let Some(surf) = self.reconciler.surfaces.get_mut(&key) else {
            return false;
        };
        let move_dispatch = surf.dispatch_pointer_move_at(pos, modifiers, emit_move);
        let mut dispatched = false;
        if let Some(tr) = move_dispatch.hover {
            if let Some(msg) = tr.on_leave {
                out_msgs.push_back(msg);
                dispatched = true;
            }
            if let Some(msg) = tr.on_hover {
                out_msgs.push_back(msg);
                dispatched = true;
            }
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            if let Some(sound) = tr.on_hover_sound {
                self.player.play_trigger(sound);
                dispatched = true;
            }
        }
        if let Some(msg) = move_dispatch.on_node_move {
            out_msgs.push_back(msg);
            dispatched = true;
        }
        if let Some(msg) = move_dispatch.on_surface_move {
            out_msgs.push_back(msg);
            dispatched = true;
        }
        dispatched
    }
}
