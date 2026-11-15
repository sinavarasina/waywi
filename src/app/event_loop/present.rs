use std::time::{Duration, Instant};

use wayland_client::Connection;

use crate::error::Result;
use crate::geometry::BufferPx;
use crate::reconcile::{ReconcileTargets, RetainedSurfaceKey};
use crate::render::{FrameScene, RenderBackend, enter_memo_scope};
use crate::wayland::WaylandState;

use super::super::runner::Application;
use super::super::subscription_mgr::{SubscriptionClients, flatten_specs};
use super::LoopRuntime;

/// Phase 4: synchronize the view tree, retained Wayland surfaces, and subscriptions.
pub(crate) fn sync_view_and_subscriptions<A: Application>(
    rt: &mut LoopRuntime<A::Message>,
    wl: &mut WaylandState,
    state: &A::State,
) -> Result<()> {
    rt.needs_view_sync = false;
    let _arena_guard = rt.arena.enter_scope();
    let now_sync = Instant::now();

    {
        let subs = A::subscriptions(state);
        let mut flat = Vec::new();
        flatten_specs(&subs, &mut flat);
        rt.timers.reconcile(flat.iter().copied(), now_sync);
        rt.system.reconcile(flat.iter().copied());
        let had_kb_subs = !rt.sub_mgr.keyboard_layout_subs.is_empty();
        rt.sub_mgr.sync(
            &flat,
            &rt.loop_handle,
            &rt.inbox,
            SubscriptionClients {
                #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
                audio: &rt.audio_client,
                #[cfg(feature = "hyprland-ipc")]
                hyprland: rt.hyprland_client.as_ref(),
                #[cfg(feature = "sway-ipc")]
                sway: rt.sway_client.as_ref(),
                #[cfg(feature = "dbus")]
                dbus: &rt.dbus_client,
                #[cfg(feature = "control-ipc")]
                control_socket_override: rt.control_socket_override.as_deref(),
                _marker: std::marker::PhantomData,
            },
        );
        if !had_kb_subs
            && !rt.sub_mgr.keyboard_layout_subs.is_empty()
            && !wl.keyboard_layouts.is_empty()
        {
            let info = wl.keyboard_layout_info();
            let mut guard = rt.inbox.borrow_mut();
            for map in &rt.sub_mgr.keyboard_layout_subs {
                guard.messages.push_back(map.call(info.clone()));
            }
        }
        #[cfg(feature = "idle-notify")]
        wl.sync_idle_watches(&rt.sub_mgr.idle_watches, &rt.qh);
        #[cfg(feature = "clipboard")]
        wl.set_clipboard_watching(!rt.sub_mgr.clipboard_subs.is_empty(), &rt.qh);
    }

    let report = {
        let _memo_guard = enter_memo_scope(&mut rt.memo_store);
        let declared = A::view(state);
        rt.reconciler.reconcile(
            declared,
            &ReconcileTargets {
                outputs: &wl.outputs,
                configured_sizes: &rt.configured_sizes,
                surface_scales: &rt.surface_scales,
            },
            &mut rt.renderer,
            &rt.assets,
            now_sync,
        )
    };
    rt.memo_store.prune_stale();

    for rk in &report.removed_surfaces {
        wl.destroy_surface(*rk);
        rt.renderer.drop_surface_target(*rk);
        rt.configured_sizes.remove(rk);
        rt.surface_scales.remove(rk);
    }
    for rk in &report.created_surfaces {
        if let Some(surf) = rt.reconciler.surfaces.get(rk) {
            wl.create_surface(&rt.qh, *rk, &surf.spec)?;
        }
    }
    for rk in &report.updated_surfaces {
        if let Some(surf) = rt.reconciler.surfaces.get(rk) {
            wl.update_surface_spec(*rk, &surf.spec)?;
        }
    }
    for (rk, surf) in &mut rt.reconciler.surfaces {
        if let Some(hv) = surf.hovered_node
            && report.retired_node_ids.contains(&hv)
        {
            surf.hovered_node = None;
        }
        if surf.input_region_dirty {
            wl.apply_input_region(*rk, surf.spec.input_mode, &surf.selective_input_rects);
            surf.input_region_dirty = false;
            if !surf.needs_redraw {
                wl.commit_if_configured(*rk);
            }
        }
        if surf.needs_redraw {
            wl.notify_surface_damage(*rk, &surf.dirty_buffer_rects(), surf.full_redraw);
        }
    }

    #[cfg(feature = "accessibility")]
    {
        rt.a11y_dirty = false;
        rt.last_a11y_focused_surface = wl.focused_keyboard_surface;
        rt.a11y_bridge
            .sync(&rt.reconciler.surfaces, wl.focused_keyboard_surface);
    }

    Ok(())
}

/// Phase 5: render and commit all configured dirty Wayland surfaces.
pub(crate) fn present_dirty_surfaces<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    wl: &mut WaylandState,
) -> Result<()> {
    let frame_seq = rt.reconciler.frame_seq;
    let renderer = &mut rt.renderer;
    let assets = &rt.assets;
    let conn = &rt.conn;

    for (&rk, surf) in &mut rt.reconciler.surfaces {
        let pending = wl.surfaces.get(&rk).is_some_and(|ws| ws.pending_redraw);
        if !surf.needs_redraw && !pending {
            continue;
        }
        let logical_size = surf.effective_logical_size();
        let scale = surf.scale;
        let scene = FrameScene {
            scale,
            background: surf.spec.background,
            commands: &surf.commands,
            assets,
            frame_seq,
        };

        let gpu_committed = if renderer.is_gpu_active() {
            wl.try_present_external_surface(&rt.qh, rk, logical_size, |wl_surface| {
                renderer.present_wayland_surface(conn, wl_surface, rk, logical_size, scene)
            })?
        } else {
            false
        };

        let committed = if gpu_committed {
            true
        } else {
            wl.present_surface_with(
                &rt.qh,
                rk,
                logical_size,
                scale,
                |pixels, bw, bh, _stride, damage| {
                    let _ = renderer.render_argb8888(
                        pixels,
                        BufferPx(bw as i32),
                        BufferPx(bh as i32),
                        damage,
                        scene,
                    );
                },
            )?
        };

        if committed {
            surf.needs_redraw = false;
            surf.full_redraw = false;
            surf.dirty_logical_rects.clear();
        }
    }

    Ok(())
}

/// Phase 6: compute the exact earliest wakeup timeout without busy polling.
pub(crate) fn compute_wakeup_timeout<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
) -> Option<Duration> {
    if !rt.inbox.borrow().messages.is_empty() {
        return Some(Duration::ZERO);
    }
    let now_sleep = Instant::now();
    let next_timer = rt.timers.next_deadline();
    let next_sys = rt.system.next_deadline(now_sleep);
    let next_anim = rt.reconciler.next_animation_deadline();
    [next_timer, next_sys, next_anim]
        .into_iter()
        .flatten()
        .min()
        .map(|dl| dl.saturating_duration_since(now_sleep))
}

/// Graceful teardown: destroy all mapped surfaces, clean up subscriptions, and flush Wayland.
pub(crate) fn shutdown_surfaces<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    wl: &mut WaylandState,
    conn: &Connection,
) {
    let active_keys: Vec<RetainedSurfaceKey> = wl.surfaces.keys().copied().collect();
    for rk in active_keys {
        wl.destroy_surface(rk);
    }
    rt.sub_mgr.cleanup(&rt.loop_handle);
    let _ = conn.flush();
}
