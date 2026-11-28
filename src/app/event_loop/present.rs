use std::time::{Duration, Instant};

use wayland_client::Connection;

use crate::asset::AssetId;
use crate::error::Result;
use crate::geometry::BufferPx;
use crate::reconcile::RetainedSurfaceKey;
use crate::render::{FrameScene, RenderBackend, WaylandPresentOutcome, WaylandPresentTarget};
use crate::surface::SurfaceKind;
use crate::wayland::WaylandState;

use super::super::runner::Application;
use super::super::subscription_mgr::{SubscriptionClients, fanout_event, flatten_specs};
use super::LoopRuntime;

/// Orders `keys` so child `xdg_popup` surfaces are always destroyed before their parent surfaces,
/// including nested popup chains (`submenu -> menu -> parent`).
fn order_keys_children_first(
    wl: &WaylandState,
    keys: &[RetainedSurfaceKey],
) -> Vec<RetainedSurfaceKey> {
    let mut remaining: Vec<RetainedSurfaceKey> = keys.to_vec();
    let mut ordered = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let mut next = Vec::new();
        let mut progressed = false;
        for &rk in &remaining {
            let has_live_child_in_set = remaining.iter().any(|&other| {
                other != rk
                    && wl
                        .surfaces
                        .get(&other)
                        .and_then(|s| s.spec.popup.as_ref())
                        .is_some_and(|p| p.parent == rk.surface_key)
            });
            if has_live_child_in_set {
                next.push(rk);
            } else {
                ordered.push(rk);
                progressed = true;
            }
        }
        if !progressed {
            ordered.extend(next);
            break;
        }
        remaining = next;
    }
    ordered
}

/// Tears down a single live Wayland surface in the required safety order:
/// 1. Drop any GPU swapchain target (`VkSurfaceKHR`) while the `wl_surface` is still alive.
/// 2. Destroy the Wayland surface and release SHM pools.
/// 3. Clear cached configure size and scale metadata.
pub(crate) fn teardown_surface<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    wl: &mut WaylandState,
    rk: RetainedSurfaceKey,
    detach_null_buffer: bool,
) {
    rt.core.renderer.drop_surface_target(rk);
    wl.destroy_surface(rk, detach_null_buffer);
    rt.core.configured_sizes.remove(&rk);
    rt.core.surface_scales.remove(&rk);
}

/// Tears down `rk` and any live transitive child `xdg_popup` surfaces (children first), retiring
/// them from the reconciler and returning `rk`'s `on_close` message if configured.
pub(crate) fn teardown_surface_with_popups<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    wl: &mut WaylandState,
    rk: RetainedSurfaceKey,
    detach_null_buffer: bool,
) -> Option<M> {
    let mut closing = vec![rk];
    loop {
        let before = closing.len();
        for (&cand_rk, cand_surf) in &wl.surfaces {
            if closing.contains(&cand_rk) {
                continue;
            }
            if let Some(p) = &cand_surf.spec.popup
                && closing.iter().any(|parent| {
                    parent.surface_key == p.parent
                        && (parent.output_id == cand_rk.output_id || cand_rk.output_id.is_none())
                })
            {
                closing.push(cand_rk);
            }
        }
        if closing.len() == before {
            break;
        }
    }

    let ordered = order_keys_children_first(wl, &closing);
    let mut root_on_close = None;
    for target_rk in ordered {
        let detach = if target_rk == rk {
            detach_null_buffer
        } else {
            false
        };
        teardown_surface(rt, wl, target_rk, detach);
        if let Some((on_close, retired)) = rt.core.reconciler.remove_surface(target_rk) {
            rt.core.renderer.drop_canvas_cache_for(&retired);
            if target_rk == rk {
                root_on_close = on_close;
            } else if let Some(msg) = on_close {
                rt.inbox.borrow_mut().messages.push_back(msg);
            }
        }
    }
    root_on_close
}
/// Phase 4: synchronize the view tree, retained Wayland surfaces, and subscriptions.
pub(crate) fn sync_view_and_subscriptions<A: Application>(
    rt: &mut LoopRuntime<A::Message>,
    wl: &mut WaylandState,
    state: &A::State,
) -> Result<()> {
    rt.needs_view_sync = false;
    let now_sync = Instant::now();

    let report = {
        let _arena_guard = rt.core.arena.enter_scope();
        {
            let subs = A::subscriptions(state);
            let mut flat = Vec::new();
            flatten_specs(&subs, &mut flat);
            rt.core.timers.reconcile(flat.iter().copied(), now_sync);
            rt.core
                .system_samplers
                .reconcile(flat.iter().copied(), &mut rt.core.system);
            let had_kb_subs = !rt.sub_mgr.keyboard_layout_subs.is_empty();
            #[cfg(feature = "foreign-toplevel")]
            let had_ft_subs = !rt.sub_mgr.foreign_toplevel_subs.is_empty();
            #[cfg(feature = "ext-workspace")]
            let had_ws_subs = !rt.sub_mgr.ext_workspace_subs.is_empty();
            rt.sub_mgr.sync(
                &flat,
                &rt.loop_handle,
                &rt.inbox,
                SubscriptionClients {
                    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
                    audio: &rt.core.audio_client,
                    #[cfg(feature = "hyprland-ipc")]
                    hyprland: rt.core.hyprland_client.as_ref(),
                    #[cfg(feature = "sway-ipc")]
                    sway: rt.core.sway_client.as_ref(),
                    #[cfg(feature = "dbus")]
                    dbus: &rt.core.dbus_client,
                    #[cfg(feature = "control-ipc")]
                    control_socket_override: rt.control_socket_override.as_deref(),
                    #[cfg(feature = "control-ipc")]
                    app_name: rt.core.app_name.as_str(),
                    _marker: std::marker::PhantomData,
                },
            );
            if !had_kb_subs
                && !rt.sub_mgr.keyboard_layout_subs.is_empty()
                && !wl.keyboard_layouts.is_empty()
            {
                let info = wl.keyboard_layout_info();
                fanout_event(
                    info,
                    &rt.sub_mgr.keyboard_layout_subs,
                    &mut rt.inbox.borrow_mut().messages,
                );
            }
            #[cfg(feature = "foreign-toplevel")]
            if !had_ft_subs
                && !rt.sub_mgr.foreign_toplevel_subs.is_empty()
                && wl.foreign_toplevel.is_supported()
            {
                let snap = wl.foreign_toplevel.snapshot(&wl.output_handles);
                fanout_event(
                    snap,
                    &rt.sub_mgr.foreign_toplevel_subs,
                    &mut rt.inbox.borrow_mut().messages,
                );
            }
            #[cfg(feature = "ext-workspace")]
            if !had_ws_subs
                && !rt.sub_mgr.ext_workspace_subs.is_empty()
                && wl.ext_workspace.is_supported()
            {
                let snap = wl.ext_workspace.snapshot(&wl.output_handles);
                fanout_event(
                    snap,
                    &rt.sub_mgr.ext_workspace_subs,
                    &mut rt.inbox.borrow_mut().messages,
                );
            }
            #[cfg(feature = "idle-notify")]
            wl.sync_idle_watches(&rt.sub_mgr.idle_watches, &rt.qh);
            #[cfg(feature = "clipboard")]
            wl.set_clipboard_watching(!rt.sub_mgr.clipboard_subs.is_empty(), &rt.qh);
        }

        rt.core
            .reconcile_view(&wl.outputs, now_sync, || A::view(state))
    };

    // Destroy xdg_popup surfaces before parent surfaces per Wayland xdg_shell ordering rules,
    // dropping each GPU swapchain before destroying its `wl_surface`.
    for rk in order_keys_children_first(wl, &report.removed_surfaces) {
        teardown_surface(rt, wl, rk, true);
    }
    // Create parent surfaces before child xdg_popup surfaces (supporting nested popups).
    let mut pending_create: Vec<RetainedSurfaceKey> = report.created_surfaces.clone();
    while !pending_create.is_empty() {
        let mut deferred = Vec::new();
        let mut progressed = false;
        for rk in pending_create {
            let Some(surf) = rt.core.reconciler.surfaces.get(&rk) else {
                continue;
            };
            let waiting_on_parent = surf
                .spec
                .popup
                .as_ref()
                .is_some_and(|p| !wl.surfaces.keys().any(|live| live.surface_key == p.parent));
            if waiting_on_parent {
                deferred.push(rk);
            } else {
                wl.create_surface(&rt.qh, rk, &surf.spec)?;
                #[cfg(feature = "toplevel-icon")]
                wl.sync_toplevel_icon(&rt.qh, rk, &rt.core.assets);
                progressed = true;
            }
        }
        if !progressed {
            for rk in deferred {
                let is_orphan_popup = rt
                    .core
                    .reconciler
                    .surfaces
                    .get(&rk)
                    .is_some_and(|s| s.spec.kind == SurfaceKind::Popup);
                if is_orphan_popup {
                    // Parent surface is currently unmapped (e.g. monitor hotplug / DPMS deep sleep
                    // closed the parent layer surface). Dismiss cleanly instead of terminating the
                    // event loop with an unhandled SurfaceError.
                    if let Some((on_close, retired)) = rt.core.reconciler.remove_surface(rk) {
                        rt.core.renderer.drop_canvas_cache_for(&retired);
                        if let Some(msg) = on_close {
                            rt.inbox.borrow_mut().messages.push_back(msg);
                        }
                    }
                    continue;
                }
                if let Some(surf) = rt.core.reconciler.surfaces.get(&rk) {
                    wl.create_surface(&rt.qh, rk, &surf.spec)?;
                    #[cfg(feature = "toplevel-icon")]
                    wl.sync_toplevel_icon(&rt.qh, rk, &rt.core.assets);
                }
            }
            break;
        }
        pending_create = deferred;
    }
    for rk in &report.updated_surfaces {
        if let Some(surf) = rt.core.reconciler.surfaces.get(rk) {
            wl.update_surface_spec(&rt.qh, *rk, &surf.spec)?;
            #[cfg(feature = "toplevel-icon")]
            wl.sync_toplevel_icon(&rt.qh, *rk, &rt.core.assets);
        }
    }
    if let Some(focus_rk) = wl.pointer_focus_surface
        && let Some(&pos) = wl.last_pointer_pos.get(&focus_rk)
        && let Some(surf) = rt.core.reconciler.surfaces.get(&focus_rk)
    {
        let shape = surf.cursor_at(pos);
        wl.apply_cursor_shape(shape);
    }
    for (rk, surf) in &mut rt.core.reconciler.surfaces {
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
            .sync(&rt.core.reconciler.surfaces, wl.focused_keyboard_surface);
    }

    Ok(())
}

/// Phase 5: render and commit all configured dirty Wayland surfaces.
pub(crate) fn present_dirty_surfaces<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    wl: &mut WaylandState,
) -> Result<()> {
    let frame_seq = rt.core.reconciler.frame_seq;
    let renderer = &mut rt.core.renderer;
    let assets = &mut rt.core.assets;
    let conn = &rt.conn;
    let mut reclaim_candidates: Vec<AssetId> = Vec::new();

    for (&rk, surf) in &mut rt.core.reconciler.surfaces {
        let Some(ws) = wl.surfaces.get(&rk) else {
            continue;
        };
        let pending = ws.pending_redraw;
        if !surf.needs_redraw && !pending {
            continue;
        }
        if !ws.configured || ws.frame_pending {
            if let Some(ws_mut) = wl.surfaces.get_mut(&rk) {
                ws_mut.pending_redraw = true;
            }
            continue;
        }
        assets.ensure_decoded_for_commands(&surf.commands);
        let logical_size = surf.effective_logical_size();
        let scale = surf.scale;
        let scene = FrameScene {
            scale,
            background: surf.spec.background,
            commands: &surf.commands,
            assets: &*assets,
            frame_seq,
        };

        let gpu_outcome = if renderer.is_gpu_active() {
            wl.try_present_external_surface(
                &rt.qh,
                rk,
                logical_size,
                |wl_surface, on_before_commit| {
                    renderer.present_wayland_surface(
                        WaylandPresentTarget {
                            conn,
                            surface: wl_surface,
                            key: rk,
                            logical_size,
                        },
                        scene,
                        on_before_commit,
                    )
                },
            )?
        } else {
            WaylandPresentOutcome::FallbackToShm
        };

        let committed = match gpu_outcome {
            WaylandPresentOutcome::Committed => true,
            WaylandPresentOutcome::Deferred => false,
            WaylandPresentOutcome::FallbackToShm => wl.present_surface_with(
                &rt.qh,
                rk,
                logical_size,
                scale,
                |pixels, bw, bh, _stride, damage| {
                    renderer.render_argb8888(
                        pixels,
                        BufferPx(bw as i32),
                        BufferPx(bh as i32),
                        damage,
                        scene,
                    )
                },
            )?,
        };

        if committed {
            surf.needs_redraw = false;
            surf.full_redraw = false;
            surf.dirty_logical_rects.clear();
            assets.collect_reclaimable_from_commands(&surf.commands, &mut reclaim_candidates);
        }
    }

    assets.reclaim_assets(&reclaim_candidates);

    // Arm VSync `wl_surface.frame` callbacks ONLY for surfaces with active retained transitions
    // (or a single configured surface when `Animation::frames` is subscribed). Zero wakeups once
    // settled or unsubscribed.
    let mut any_frame_pending = wl
        .surfaces
        .values()
        .any(|s| s.configured && s.frame_pending);
    for (&rk, surf) in &rt.core.reconciler.surfaces {
        if surf.has_active_transitions && wl.arm_frame_callback(&rt.qh, rk) {
            any_frame_pending = true;
        }
    }
    if !rt.sub_mgr.animation_frame_subs.is_empty() && !any_frame_pending {
        let first_configured = wl
            .surfaces
            .iter()
            .find(|(_, s)| s.configured && !s.frame_pending)
            .map(|(&rk, _)| rk);
        if let Some(rk) = first_configured {
            wl.arm_frame_callback(&rt.qh, rk);
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
    rt.core
        .next_core_deadline(now_sleep)
        .map(|dl| dl.saturating_duration_since(now_sleep))
}

/// Graceful teardown: destroy all mapped surfaces (popups first), clean up subscriptions, and flush Wayland.
pub(crate) fn shutdown_surfaces<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    wl: &mut WaylandState,
    conn: &Connection,
) {
    let active_keys: Vec<RetainedSurfaceKey> = wl.surfaces.keys().copied().collect();
    for rk in order_keys_children_first(wl, &active_keys) {
        teardown_surface(rt, wl, rk, true);
    }
    wl.dnd.cleanup();
    #[cfg(feature = "clipboard")]
    wl.clipboard.cleanup();
    rt.sub_mgr.cleanup(&rt.loop_handle);
    rt.core.system.stop_idle_workers();
    let _ = conn.flush();
}
