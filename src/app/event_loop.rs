pub(crate) mod present;
pub(crate) mod sources;
pub(crate) mod wayland_dispatch;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::ping as calloop_ping;
use calloop::{EventLoop, LoopHandle};
use calloop_wayland_source::WaylandSource;
use wayland_client::{Connection, QueueHandle};

use crate::arena::FrameArenaPool;
use crate::asset::{AssetCache, AssetId};
use crate::capabilities::{Capabilities, Capability};
use crate::effect::{AsyncAssetWorker, BoxedActivationCallback, BoxedMsgFactory, EffectCtx};
use crate::error::{Result, WaywiError};
use crate::geometry::{LogicalSize, Point, ScaleFactor};
use crate::reconcile::{Reconciler, RetainedSurfaceKey};
#[cfg(feature = "text")]
use crate::render::RenderBackend;
use crate::render::{ActiveRenderer, MemoStore, RendererPreference};
use crate::system::SystemProviders;
use crate::timer::TimerScheduler;
use crate::wayland::WaylandState;

use super::runner::{App, Application};
use super::subscription_mgr::{LoopInbox, SubscriptionManager};

pub(crate) struct LoopRuntime<M: Clone + 'static> {
    pub(crate) conn: Connection,
    pub(crate) qh: QueueHandle<WaylandState>,
    pub(crate) loop_handle: LoopHandle<'static, WaylandState>,
    pub(crate) assets: AssetCache,
    pub(crate) asset_worker: AsyncAssetWorker,
    pub(crate) system: SystemProviders,
    pub(crate) renderer: ActiveRenderer,
    pub(crate) arena: FrameArenaPool,
    pub(crate) memo_store: MemoStore,
    pub(crate) reconciler: Reconciler<M>,
    pub(crate) timers: TimerScheduler<M>,
    pub(crate) capabilities: Capabilities,
    pub(crate) configured_sizes: HashMap<RetainedSurfaceKey, LogicalSize>,
    pub(crate) surface_scales: HashMap<RetainedSurfaceKey, ScaleFactor>,
    pub(crate) touch_positions: HashMap<i32, Point>,
    pub(crate) next_activation_req_id: u64,
    pub(crate) pending_activation_callbacks: HashMap<u64, BoxedActivationCallback>,
    pub(crate) inbox: Rc<RefCell<LoopInbox<M>>>,
    pub(crate) sub_mgr: SubscriptionManager<M>,
    pub(crate) quit_requested: bool,
    pub(crate) needs_view_sync: bool,
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    pub(crate) audio_client: crate::audio::AudioClient,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub(crate) player: crate::audio::AudioPlayer,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_socket_override: Option<std::path::PathBuf>,
    #[cfg(feature = "hyprland-ipc")]
    pub(crate) hyprland_client: Option<crate::hyprland::HyprlandClient>,
    #[cfg(feature = "sway-ipc")]
    pub(crate) sway_client: Option<crate::sway::SwayClient>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus_client: crate::dbus::DbusClient,
    #[cfg(feature = "accessibility")]
    pub(crate) a11y_bridge: crate::accessibility::AccessibilityBridge,
    #[cfg(feature = "accessibility")]
    pub(crate) a11y_dirty: bool,
    #[cfg(feature = "accessibility")]
    pub(crate) last_a11y_focused_surface: Option<RetainedSurfaceKey>,
}

impl<M: Clone + 'static> LoopRuntime<M> {
    fn new(
        conn: Connection,
        wl: &mut WaylandState,
        qh: QueueHandle<WaylandState>,
        loop_handle: LoopHandle<'static, WaylandState>,
        ping_tx: calloop_ping::Ping,
        renderer_pref: RendererPreference,
        app_name: String,
    ) -> Self {
        #[cfg(feature = "clipboard")]
        wl.clipboard.set_wake_ping(ping_tx.clone());
        #[allow(unused_mut)]
        let mut capabilities = wl.capabilities();

        let renderer = ActiveRenderer::from_preference(renderer_pref, Some(&conn));
        capabilities.set(
            crate::capabilities::Capability::GpuRenderer,
            renderer.is_gpu_active(),
        );

        let asset_worker = AsyncAssetWorker::new();
        {
            let p = ping_tx.clone();
            asset_worker.set_wakeup(move || p.ping());
        }

        #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
        let audio_client = {
            let client = crate::audio::AudioClient::new();
            let p = ping_tx.clone();
            client.set_wakeup(move || p.ping());
            client
        };

        #[cfg(feature = "hyprland-ipc")]
        let hyprland_client = {
            let client = crate::hyprland::HyprlandClient::from_env().ok();
            capabilities.set(
                crate::capabilities::Capability::HyprlandIpc,
                client.is_some(),
            );
            client
        };

        #[cfg(feature = "sway-ipc")]
        let sway_client = {
            let client = crate::sway::SwayClient::from_env().ok();
            capabilities.set(crate::capabilities::Capability::SwayIpc, client.is_some());
            client
        };

        #[cfg(feature = "dbus")]
        let dbus_client = crate::dbus::DbusClient::new();
        #[cfg(feature = "global-shortcuts")]
        capabilities.set(
            crate::capabilities::Capability::GlobalShortcuts,
            dbus_client.has_global_shortcuts_portal(),
        );
        #[cfg(feature = "accessibility")]
        let a11y_bridge = {
            let p = ping_tx.clone();
            capabilities.set(
                crate::capabilities::Capability::Accessibility,
                crate::accessibility::atspi_runtime_available(),
            );
            crate::accessibility::AccessibilityBridge::new(app_name, move || p.ping())
        };
        #[cfg(not(feature = "accessibility"))]
        let _ = app_name;

        Self {
            conn,
            qh,
            loop_handle,
            assets: AssetCache::new(),
            asset_worker,
            system: SystemProviders::new(),
            renderer,
            arena: FrameArenaPool::new(),
            memo_store: MemoStore::default(),
            reconciler: Reconciler::new(),
            timers: TimerScheduler::new(),
            capabilities,
            configured_sizes: HashMap::new(),
            surface_scales: HashMap::new(),
            touch_positions: HashMap::new(),
            next_activation_req_id: 1,
            pending_activation_callbacks: HashMap::new(),
            inbox: Rc::new(RefCell::new(LoopInbox::default())),
            sub_mgr: SubscriptionManager::new(),
            quit_requested: false,
            needs_view_sync: true,
            #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
            audio_client,
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            player: crate::audio::AudioPlayer::new(),
            #[cfg(feature = "control-ipc")]
            control_socket_override: None,
            #[cfg(feature = "hyprland-ipc")]
            hyprland_client,
            #[cfg(feature = "sway-ipc")]
            sway_client,
            #[cfg(feature = "dbus")]
            dbus_client,
            #[cfg(feature = "accessibility")]
            a11y_bridge,
            #[cfg(feature = "accessibility")]
            a11y_dirty: true,
            #[cfg(feature = "accessibility")]
            last_a11y_focused_surface: None,
        }
    }

    fn run_effect<R>(
        &mut self,
        wl: &mut WaylandState,
        f: impl FnOnce(&mut EffectCtx<'_>) -> R,
    ) -> R {
        let mut pending_after: Vec<(Duration, BoxedMsgFactory)> = Vec::new();
        let mut reloaded_assets: Vec<(AssetId, u64)> = Vec::new();
        let mut pending_activation_requests: Vec<(Option<String>, BoxedActivationCallback)> =
            Vec::new();
        let mut pending_surface_activations: Vec<(crate::surface::SurfaceKey, String)> = Vec::new();
        let outputs_copy = wl.outputs.clone();
        #[cfg(feature = "clipboard")]
        let qh_ref = &self.qh;
        #[cfg(feature = "clipboard")]
        let mut set_clip = |sel: crate::clipboard::ClipboardSelection,
                            entries: Vec<(String, Vec<u8>)>| {
            wl.set_clipboard_selection(sel, entries, qh_ref)
        };
        #[cfg(feature = "text")]
        let frame_seq = self.reconciler.frame_seq;
        #[cfg(feature = "text")]
        let mut measure_cursor = |content: &crate::text::SharedStr,
                                  style: crate::text::TextMeasureStyle<'_>,
                                  cursor_byte: usize| {
            self.renderer
                .measure_text_cursor(content, style, cursor_byte, frame_seq)
        };
        let res = {
            let mut ctx = EffectCtx {
                quit_requested: &mut self.quit_requested,
                capabilities: &self.capabilities,
                outputs: &outputs_copy,
                assets: &mut self.assets,
                asset_worker: &self.asset_worker,
                system: &mut self.system,
                pending_after: &mut pending_after,
                reloaded_assets: &mut reloaded_assets,
                pending_activation_requests: &mut pending_activation_requests,
                pending_surface_activations: &mut pending_surface_activations,
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
                clipboard_writer: Some(&mut set_clip),
                #[cfg(feature = "text")]
                text_cursor_measurer: &mut measure_cursor,
            };
            f(&mut ctx)
        };
        let now = Instant::now();
        for (delay, factory) in pending_after {
            if let Ok(msg) = factory().downcast::<M>() {
                self.timers.push_imperative(now + delay, *msg);
            }
        }
        for (aid, rev) in reloaded_assets {
            self.reconciler.invalidate_asset(aid, rev);
        }
        let mut flush_wayland = false;
        for (app_id, cb) in pending_activation_requests {
            let req_id = self.next_activation_req_id;
            self.next_activation_req_id = self.next_activation_req_id.wrapping_add(1);
            if wl.request_activation_token(&self.qh, req_id, app_id) {
                self.pending_activation_callbacks.insert(req_id, cb);
                flush_wayland = true;
            } else if let Ok(msg) = cb(Err(WaywiError::UnsupportedCapability(
                Capability::XdgActivation,
            )))
            .downcast::<M>()
            {
                self.inbox.borrow_mut().messages.push_back(*msg);
            }
        }
        for (surface_key, token) in pending_surface_activations {
            if wl.activate_surface(surface_key, token) {
                flush_wayland = true;
            }
        }
        if flush_wayland {
            let _ = self.conn.flush();
        }
        res
    }
}

pub(crate) fn run_event_loop<A: Application>(app_cfg: App, conn: Connection) -> Result<()> {
    let app_name = app_cfg.name.unwrap_or_else(|| "waywi".to_string());

    let (mut wl, queue, qh) = WaylandState::init(&conn)?;
    let mut event_loop: EventLoop<'static, WaylandState> = EventLoop::try_new().map_err(|e| {
        WaywiError::WaylandConnection(format!("calloop EventLoop init failed: {e}"))
    })?;
    let loop_handle = event_loop.handle();

    WaylandSource::new(conn.clone(), queue)
        .insert(loop_handle.clone())
        .map_err(|e| WaywiError::WaylandConnection(format!("WaylandSource insert failed: {e}")))?;

    let (ping_tx, ping_rx) = calloop_ping::make_ping()
        .map_err(|e| WaywiError::WaylandConnection(format!("ping source creation failed: {e}")))?;
    let _ping_token = loop_handle
        .insert_source(ping_rx, |_, _, _| {})
        .map_err(|e| WaywiError::WaylandConnection(format!("ping insert failed: {e}")))?;

    let mut rt: LoopRuntime<A::Message> = LoopRuntime::new(
        conn.clone(),
        &mut wl,
        qh,
        loop_handle,
        ping_tx,
        app_cfg.renderer_preference,
        app_name,
    );
    #[cfg(feature = "control-ipc")]
    {
        rt.control_socket_override = app_cfg.control_socket_override;
    }
    let mut state = rt.run_effect(&mut wl, |ctx| A::init(ctx))?;

    if rt.quit_requested {
        rt.sub_mgr.cleanup(&rt.loop_handle);
        return Ok(());
    }

    loop {
        let now = Instant::now();
        sources::collect_sources(&mut rt, &mut wl, now);
        wayland_dispatch::drain_wayland_events(&mut rt, &mut wl);

        // Phase 3: process queued application messages through update(state, msg, ctx).
        let mut pending_msgs = std::mem::take(&mut rt.inbox.borrow_mut().messages);
        if !pending_msgs.is_empty() {
            rt.needs_view_sync = true;
            for msg in pending_msgs.drain(..) {
                rt.run_effect(&mut wl, |ctx| A::update(&mut state, msg, ctx))?;
                if rt.quit_requested {
                    break;
                }
            }
        }
        if rt.quit_requested {
            break;
        }

        if rt.needs_view_sync {
            present::sync_view_and_subscriptions::<A>(&mut rt, &mut wl, &state)?;
        } else {
            #[cfg(feature = "accessibility")]
            if rt.a11y_dirty {
                rt.a11y_dirty = false;
                rt.last_a11y_focused_surface = wl.focused_keyboard_surface;
                rt.a11y_bridge
                    .sync(&rt.reconciler.surfaces, wl.focused_keyboard_surface);
            }
        }
        present::present_dirty_surfaces(&mut rt, &mut wl)?;
        let _ = conn.flush();

        let timeout = present::compute_wakeup_timeout(&mut rt);
        event_loop
            .dispatch(timeout, &mut wl)
            .map_err(|e| WaywiError::WaylandConnection(format!("calloop dispatch failed: {e}")))?;
    }

    present::shutdown_surfaces(&mut rt, &mut wl, &conn);
    Ok(())
}
