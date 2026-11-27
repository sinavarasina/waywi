pub(crate) mod present;
pub(crate) mod sources;
pub(crate) mod wayland_dispatch;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use calloop::ping as calloop_ping;
use calloop::{EventLoop, LoopHandle};
use calloop_wayland_source::WaylandSource;
use wayland_client::{Connection, QueueHandle};

use crate::capabilities::Capability;
#[cfg(feature = "screencopy")]
use crate::effect::ScreencopyCallback;
use crate::effect::{ActivationCallback, EffectCtx};
use crate::error::{Result, WaywiError};
#[cfg(feature = "accessibility")]
use crate::reconcile::RetainedSurfaceKey;
use crate::text::SharedStr;
use crate::wayland::WaylandState;

use super::core::CoreEngine;
use super::runner::{App, Application};
use super::subscription_mgr::{LoopInbox, SubscriptionManager};

pub(crate) struct LoopRuntime<M: Clone + 'static> {
    pub(crate) conn: Connection,
    pub(crate) qh: QueueHandle<WaylandState>,
    pub(crate) loop_handle: LoopHandle<'static, WaylandState>,
    pub(crate) core: CoreEngine<M>,
    pub(crate) next_activation_req_id: u64,
    pub(crate) pending_activation_callbacks: HashMap<u64, ActivationCallback<M>>,
    #[cfg(feature = "screencopy")]
    pub(crate) next_screencopy_req_id: u64,
    #[cfg(feature = "screencopy")]
    pub(crate) pending_screencopy_callbacks: HashMap<u64, ScreencopyCallback<M>>,
    pub(crate) inbox: Rc<RefCell<LoopInbox<M>>>,
    pub(crate) sub_mgr: SubscriptionManager<M>,
    pub(crate) needs_view_sync: bool,
    #[cfg(feature = "control-ipc")]
    pub(crate) control_socket_override: Option<std::path::PathBuf>,
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
        app_cfg: App,
        app_name: String,
    ) -> Result<Self> {
        wl.dnd.set_loop_handle(loop_handle.clone());
        #[cfg(feature = "clipboard")]
        wl.clipboard.set_loop_handle(loop_handle.clone());
        #[allow(unused_mut)]
        let mut capabilities = wl.capabilities();

        #[cfg(feature = "accessibility")]
        let a11y_bridge = {
            let p = ping_tx.clone();
            capabilities.set(
                Capability::Accessibility,
                crate::accessibility::atspi_runtime_available(),
            );
            crate::accessibility::AccessibilityBridge::new(app_name.clone(), move || p.ping())
        };

        let core = CoreEngine::new(
            capabilities,
            app_cfg.renderer_preference,
            Some(&conn),
            app_cfg.memory_config,
            SharedStr::from(app_name),
            true,
        )?;
        {
            let p = ping_tx.clone();
            core.asset_worker.set_wakeup(move || p.ping());
        }
        #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
        {
            let p = ping_tx.clone();
            core.audio_client.set_wakeup(move || p.ping());
        }

        Ok(Self {
            conn,
            qh,
            loop_handle,
            core,
            next_activation_req_id: 1,
            pending_activation_callbacks: HashMap::new(),
            #[cfg(feature = "screencopy")]
            next_screencopy_req_id: 1,
            #[cfg(feature = "screencopy")]
            pending_screencopy_callbacks: HashMap::new(),
            inbox: Rc::new(RefCell::new(LoopInbox::default())),
            sub_mgr: SubscriptionManager::new(),
            needs_view_sync: true,
            #[cfg(feature = "control-ipc")]
            control_socket_override: app_cfg.control_socket_override,
            #[cfg(feature = "accessibility")]
            a11y_bridge,
            #[cfg(feature = "accessibility")]
            a11y_dirty: true,
            #[cfg(feature = "accessibility")]
            last_a11y_focused_surface: None,
        })
    }

    fn run_effect<R>(
        &mut self,
        wl: &mut WaylandState,
        f: impl FnOnce(&mut EffectCtx<'_, M>) -> R,
    ) -> R {
        let now = Instant::now();
        let (res, post) = {
            #[cfg(feature = "clipboard")]
            let (outputs_slice, clipboard, seat_state) =
                (&wl.outputs[..], &mut wl.clipboard, &wl.seat_state);
            #[cfg(not(feature = "clipboard"))]
            let outputs_slice = &wl.outputs[..];

            #[cfg(feature = "clipboard")]
            let qh_ref = &self.qh;
            #[cfg(feature = "clipboard")]
            let mut set_clip = |sel: crate::clipboard::ClipboardSelection,
                                entries: Vec<(String, Vec<u8>)>| {
                let seat = seat_state.seats().next();
                clipboard.set_selection(seat.as_ref(), sel, entries, qh_ref)
            };

            self.core.run_effect(
                outputs_slice,
                #[cfg(feature = "clipboard")]
                Some(&mut set_clip),
                now,
                f,
            )
        };

        if post.trim_requested {
            wl.trim_idle_shm_pools();
        }

        let mut flush_wayland = false;
        for (app_id, cb) in post.activation_requests {
            let req_id = self.next_activation_req_id;
            self.next_activation_req_id = self.next_activation_req_id.wrapping_add(1);
            if wl.request_activation_token(&self.qh, req_id, app_id) {
                self.pending_activation_callbacks.insert(req_id, cb);
                flush_wayland = true;
            } else {
                self.inbox.borrow_mut().messages.push_back(cb(Err(
                    WaywiError::UnsupportedCapability(Capability::XdgActivation),
                )));
            }
        }
        for (surface_key, token) in post.surface_activations {
            if wl.activate_surface(surface_key, token) {
                flush_wayland = true;
            }
        }
        for (target, overlay_cursor, cb) in post.screencopy_requests {
            #[cfg(feature = "screencopy")]
            {
                let req_id = self.next_screencopy_req_id;
                self.next_screencopy_req_id = self.next_screencopy_req_id.wrapping_add(1).max(1);
                match wl.start_screencopy(&self.qh, req_id, &target, overlay_cursor) {
                    Ok(()) => {
                        self.pending_screencopy_callbacks.insert(req_id, cb);
                        flush_wayland = true;
                    }
                    Err(err) => {
                        self.inbox.borrow_mut().messages.push_back(cb(Err(err)));
                    }
                }
            }
            #[cfg(not(feature = "screencopy"))]
            {
                let _ = (target, overlay_cursor);
                self.inbox.borrow_mut().messages.push_back(cb(Err(
                    WaywiError::UnsupportedCapability(Capability::Screencopy),
                )));
            }
        }
        #[cfg(feature = "foreign-toplevel")]
        for action in post.toplevel_actions {
            if wl.apply_foreign_toplevel_action(&action) {
                flush_wayland = true;
            }
        }
        #[cfg(feature = "ext-workspace")]
        {
            let mut ws_acted = false;
            for action in post.ext_workspace_actions {
                if wl.apply_ext_workspace_action(&action) {
                    ws_acted = true;
                }
            }
            if ws_acted {
                wl.commit_ext_workspace();
                flush_wayland = true;
            }
        }
        #[cfg(feature = "virtual-keyboard")]
        for action in post.virtual_keyboard_actions {
            if wl.apply_virtual_keyboard_action(&self.qh, &action).is_ok() {
                flush_wayland = true;
            }
        }
        for drag in post.drag_starts {
            if wl.start_drag(&self.qh, drag) {
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
    let app_name = app_cfg.name.clone().unwrap_or_else(|| "waywi".to_string());

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
        app_cfg,
        app_name,
    )?;

    let mut state = rt.run_effect(&mut wl, |ctx| A::init(ctx))?;

    if rt.core.quit_requested {
        rt.sub_mgr.cleanup(&rt.loop_handle);
        rt.core.system.stop_idle_workers();
        return Ok(());
    }

    // Establish initial surfaces and subscriptions from `init()`'s returned state before
    // collecting timers or dispatching `update()` messages so slow `init()` effects cannot cause
    // short `ctx.after(...)` timers to skip the initial view state.
    present::sync_view_and_subscriptions::<A>(&mut rt, &mut wl, &state)?;
    present::present_dirty_surfaces(&mut rt, &mut wl)?;
    let _ = conn.flush();

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
                if rt.core.quit_requested {
                    break;
                }
            }
        }
        if rt.core.quit_requested {
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
                    .sync(&rt.core.reconciler.surfaces, wl.focused_keyboard_surface);
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
