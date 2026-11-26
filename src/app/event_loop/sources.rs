use std::time::Instant;

use crate::wayland::WaylandState;

#[cfg(any(
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa",
    feature = "hyprland-ipc",
    feature = "sway-ipc",
))]
use super::super::subscription_mgr::fanout_event;
use super::LoopRuntime;

#[cfg(any(
    feature = "hyprland-ipc",
    feature = "sway-ipc",
    feature = "dbus",
    feature = "global-shortcuts",
    feature = "control-ipc",
))]
fn push_mapped<M: Clone + 'static, E, F>(
    inbox: &std::rc::Rc<std::cell::RefCell<crate::app::subscription_mgr::LoopInbox<M>>>,
    events: Vec<E>,
    mut emit: F,
) where
    F: FnMut(E, &mut std::collections::VecDeque<M>),
{
    if events.is_empty() {
        return;
    }
    let mut guard = inbox.borrow_mut();
    for ev in events {
        emit(ev, &mut guard.messages);
    }
}

/// Phase 1: collect due timers, provider samples, animation frames, async assets, and IPC inputs.
pub(crate) fn collect_sources<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    wl: &mut WaylandState,
    now: Instant,
) {
    let anim_dirty = {
        let mut guard = rt.inbox.borrow_mut();
        rt.core.collect_core_due(now, &mut guard.messages)
    };
    for rk in anim_dirty {
        if let Some(s) = rt.core.reconciler.surfaces.get(&rk) {
            let buf_rects = s.dirty_buffer_rects();
            wl.notify_surface_damage(rk, &buf_rects, s.full_redraw);
        }
    }

    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    if rt.core.audio_client.has_pending() {
        let (snap_opt, events) = rt.core.audio_client.drain_updates();
        if snap_opt.is_some() || !events.is_empty() {
            let mut guard = rt.inbox.borrow_mut();
            if let Some(snap) = snap_opt {
                fanout_event(snap, &rt.sub_mgr.audio_snapshot_subs, &mut guard.messages);
            }
            for ev in events {
                fanout_event(ev, &rt.sub_mgr.audio_event_subs, &mut guard.messages);
            }
        }
    }

    #[cfg(feature = "hyprland-ipc")]
    {
        let evs: Vec<_> = rt.inbox.borrow_mut().hyprland_events.drain(..).collect();
        let emit_compositor =
            rt.core.hyprland_client.is_some() && !rt.sub_mgr.compositor_subs.is_empty();
        push_mapped(&rt.inbox, evs, |ev, messages| {
            if emit_compositor
                && let Some(ce) = crate::compositor::CompositorEvent::from_hyprland(&ev)
            {
                fanout_event(ce, &rt.sub_mgr.compositor_subs, messages);
            }
            fanout_event(ev, &rt.sub_mgr.hyprland_subs, messages);
        });
    }

    #[cfg(feature = "sway-ipc")]
    {
        let evs: Vec<_> = rt.inbox.borrow_mut().sway_events.drain(..).collect();
        #[cfg(feature = "hyprland-ipc")]
        let hyprland_primary = rt.core.hyprland_client.is_some();
        #[cfg(not(feature = "hyprland-ipc"))]
        let hyprland_primary = false;
        let emit_compositor = !hyprland_primary
            && rt.core.sway_client.is_some()
            && !rt.sub_mgr.compositor_subs.is_empty();
        push_mapped(&rt.inbox, evs, |ev, messages| {
            if emit_compositor && let Some(ce) = crate::compositor::CompositorEvent::from_sway(&ev)
            {
                fanout_event(ce, &rt.sub_mgr.compositor_subs, messages);
            }
            fanout_event(ev, &rt.sub_mgr.sway_subs, messages);
        });
    }

    #[cfg(feature = "dbus")]
    {
        let sig_evs: Vec<_> = rt.inbox.borrow_mut().dbus_signal_events.drain(..).collect();
        push_mapped(&rt.inbox, sig_evs, |(filter_key, ev), messages| {
            for (sub_k, map) in &rt.sub_mgr.dbus_signal_subs {
                if *sub_k == filter_key {
                    messages.push_back(map.call(ev.clone()));
                }
            }
        });

        let mut guard = rt.inbox.borrow_mut();
        let msgs = &mut guard.messages;
        rt.sub_mgr.dbus_notifications.drain_into(msgs);
        rt.sub_mgr.dbus_tray.drain_into(msgs);
        rt.sub_mgr.dbus_power_profiles.drain_into(msgs);
        rt.sub_mgr.dbus_network_manager.drain_into(msgs);
        rt.sub_mgr.dbus_upower.drain_into(msgs);
        rt.sub_mgr.dbus_udisks.drain_into(msgs);
        rt.sub_mgr.dbus_logind.drain_into(msgs);
        rt.sub_mgr.dbus_bluetooth.drain_into(msgs);
        rt.sub_mgr.dbus_bluetooth_agent.drain_into(msgs);
        rt.sub_mgr.dbus_modem.drain_into(msgs);
        rt.sub_mgr.dbus_screensaver.drain_into(msgs);
        rt.sub_mgr.dbus_geoclue.drain_into(msgs);
        rt.sub_mgr.dbus_portal_settings.drain_into(msgs);
        rt.sub_mgr.dbus_mpris.drain_into(msgs);
        rt.sub_mgr.dbus_action_results.drain_into(msgs);
        rt.sub_mgr.dbus_nm_secret_agent.drain_into(msgs);
        rt.sub_mgr.dbus_sensors.drain_into(msgs);
        rt.sub_mgr.dbus_polkit_agent.drain_into(msgs);
        rt.sub_mgr.dbus_systemd_user.drain_into(msgs);
        rt.sub_mgr.dbus_systemd_system.drain_into(msgs);
        rt.sub_mgr.dbus_timedate.drain_into(msgs);
        rt.sub_mgr.dbus_accounts.drain_into(msgs);
        rt.sub_mgr.dbus_fprint_verify.drain_into(msgs);
        rt.sub_mgr.dbus_gamemode.drain_into(msgs);
        rt.sub_mgr.dbus_fwupd.drain_into(msgs);
        rt.sub_mgr.dbus_cups.drain_into(msgs);
        rt.sub_mgr.dbus_kdeconnect.drain_into(msgs);
        rt.sub_mgr.dbus_colord.drain_into(msgs);
    }

    #[cfg(feature = "global-shortcuts")]
    {
        let shortcuts: Vec<_> = rt
            .inbox
            .borrow_mut()
            .global_shortcut_events
            .drain(..)
            .collect();
        push_mapped(&rt.inbox, shortcuts, |(filter_key, ev), messages| {
            for (sub_k, _, _, map) in &rt.sub_mgr.global_shortcut_subs {
                if *sub_k == filter_key {
                    messages.push_back(map.call(ev.clone()));
                }
            }
        });
    }

    #[cfg(feature = "control-ipc")]
    {
        let sigs: Vec<_> = rt.inbox.borrow_mut().control_signals.drain(..).collect();
        push_mapped(&rt.inbox, sigs, |sig, messages| {
            for (expected, msg) in &rt.sub_mgr.control_signal_subs {
                if *expected == sig {
                    messages.push_back(msg.clone());
                }
            }
        });
        let cmds: Vec<_> = rt.inbox.borrow_mut().control_commands.drain(..).collect();
        push_mapped(&rt.inbox, cmds, |(sock_path, req), messages| {
            for (sub_path, map) in &rt.sub_mgr.control_socket_subs {
                if *sub_path == sock_path {
                    messages.push_back(map.call(req.clone()));
                }
            }
        });
    }

    #[cfg(feature = "accessibility")]
    {
        let mut guard = rt.inbox.borrow_mut();
        if rt
            .a11y_bridge
            .drain_and_dispatch(&rt.core.reconciler.surfaces, &mut guard.messages)
        {
            rt.a11y_dirty = true;
        }
    }
}
