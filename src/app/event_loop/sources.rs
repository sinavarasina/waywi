use std::time::Instant;

use crate::wayland::WaylandState;

use super::LoopRuntime;

#[cfg(any(
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa",
    feature = "hyprland-ipc",
    feature = "sway-ipc",
    feature = "dbus",
    feature = "control-ipc"
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

#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc", feature = "dbus"))]
fn fanout_broadcast<M: Clone + 'static, E: Clone>(
    inbox: &std::rc::Rc<std::cell::RefCell<crate::app::subscription_mgr::LoopInbox<M>>>,
    drain: impl FnOnce(&mut crate::app::subscription_mgr::LoopInbox<M>) -> Vec<E>,
    subs: &[crate::arena::FrameSharedFn<E, M>],
) {
    let events = drain(&mut inbox.borrow_mut());
    push_mapped(inbox, events, |ev, messages| {
        for map in subs {
            messages.push_back(map.call(ev.clone()));
        }
    });
}

/// Phase 1: collect due timers, provider samples, animation frames, async assets, and IPC inputs.
pub(crate) fn collect_sources<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    wl: &mut WaylandState,
    now: Instant,
) {
    {
        let mut guard = rt.inbox.borrow_mut();
        guard.messages.extend(rt.timers.collect_due(now));
        guard.messages.extend(rt.system.collect_due::<M>(now));
    }

    let (anim_dirty, _) = rt.reconciler.tick_animations(now);
    for rk in anim_dirty {
        if let Some(s) = rt.reconciler.surfaces.get(&rk) {
            let buf_rects = s.dirty_buffer_rects();
            wl.notify_surface_damage(rk, &buf_rects, s.full_redraw);
        }
    }

    for done in rt.asset_worker.drain_completed() {
        let asset_res = done.decoded.map(|dec| {
            rt.assets
                .commit_worker_decoded(Some(done.path), done.content_hash, dec)
        });
        if let Ok(msg) = (done.map)(asset_res).downcast::<M>() {
            rt.inbox.borrow_mut().messages.push_back(*msg);
        }
    }

    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    {
        let events = rt.audio_client.drain_events();
        push_mapped(&rt.inbox, events, |ev, messages| {
            if let crate::audio::AudioEvent::Updated(ref snap) = ev {
                for map in &rt.sub_mgr.audio_snapshot_subs {
                    messages.push_back(map.call(snap.clone()));
                }
            }
            for map in &rt.sub_mgr.audio_event_subs {
                messages.push_back(map.call(ev.clone()));
            }
        });
    }

    #[cfg(feature = "hyprland-ipc")]
    fanout_broadcast(
        &rt.inbox,
        |inbox| inbox.hyprland_events.drain(..).collect(),
        &rt.sub_mgr.hyprland_subs,
    );

    #[cfg(feature = "sway-ipc")]
    fanout_broadcast(
        &rt.inbox,
        |inbox| inbox.sway_events.drain(..).collect(),
        &rt.sub_mgr.sway_subs,
    );

    #[cfg(feature = "dbus")]
    {
        let evs: Vec<_> = rt.inbox.borrow_mut().dbus_signal_events.drain(..).collect();
        push_mapped(&rt.inbox, evs, |(filter_key, ev), messages| {
            for (sub_k, map) in &rt.sub_mgr.dbus_signal_subs {
                if *sub_k == filter_key {
                    messages.push_back(map.call(ev.clone()));
                }
            }
        });
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_notifications.drain(..).collect(),
            &rt.sub_mgr.dbus_notification_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_tray.drain(..).collect(),
            &rt.sub_mgr.dbus_tray_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_power_profiles.drain(..).collect(),
            &rt.sub_mgr.dbus_power_profile_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_network_manager.drain(..).collect(),
            &rt.sub_mgr.dbus_network_manager_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_upower.drain(..).collect(),
            &rt.sub_mgr.dbus_upower_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_logind.drain(..).collect(),
            &rt.sub_mgr.dbus_logind_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_bluetooth.drain(..).collect(),
            &rt.sub_mgr.dbus_bluetooth_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_bluetooth_agent.drain(..).collect(),
            &rt.sub_mgr.dbus_bluetooth_agent_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_modem.drain(..).collect(),
            &rt.sub_mgr.dbus_modem_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_screensaver.drain(..).collect(),
            &rt.sub_mgr.dbus_screensaver_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_geoclue.drain(..).collect(),
            &rt.sub_mgr.dbus_geoclue_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_mpris.drain(..).collect(),
            &rt.sub_mgr.dbus_mpris_subs,
        );
        fanout_broadcast(
            &rt.inbox,
            |inbox| inbox.dbus_action_results.drain(..).collect(),
            &rt.sub_mgr.dbus_action_result_subs,
        );
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
        push_mapped(&rt.inbox, cmds, |req, messages| {
            for (_, map) in &rt.sub_mgr.control_socket_subs {
                if let Some(msg) = map.call(req.command().to_owned()) {
                    messages.push_back(msg);
                }
            }
            for (_, map) in &rt.sub_mgr.control_request_subs {
                if let Some(msg) = map.call(req.clone()) {
                    messages.push_back(msg);
                }
            }
        });
    }

    #[cfg(feature = "accessibility")]
    {
        let mut guard = rt.inbox.borrow_mut();
        if rt
            .a11y_bridge
            .drain_and_dispatch(&rt.reconciler.surfaces, &mut guard.messages)
        {
            rt.a11y_dirty = true;
        }
    }
}
