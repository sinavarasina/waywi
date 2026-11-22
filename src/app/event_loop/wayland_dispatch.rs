use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use crate::app::subscription_mgr::LoopInbox;
use crate::geometry::{LogicalPx, Point};
use crate::input::{Key, KeyEvent, Modifiers, MouseButton, ScrollEvent};
use crate::reconcile::{PointerDispatch, RetainedSurfaceKey};
use crate::render::RenderBackend;
use crate::wayland::{WaylandEvent, WaylandState};

use super::LoopRuntime;

fn push_message<M: Clone + 'static>(inbox: &Rc<RefCell<LoopInbox<M>>>, msg: M) {
    inbox.borrow_mut().messages.push_back(msg);
}

fn enqueue_pointer_dispatch<M: Clone + 'static>(rt: &LoopRuntime<M>, d: PointerDispatch<M>) {
    if let Some(msg) = d.on_click {
        push_message(&rt.inbox, msg);
    }
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    if let Some((sound, vol)) = d.click_sound {
        rt.player.play_with_volume(sound, vol);
    }
    if let Some(msg) = d.on_pointer {
        push_message(&rt.inbox, msg);
    }
    if let Some(msg) = d.on_surface {
        push_message(&rt.inbox, msg);
    }
}

fn dispatch_pointer_hover_and_move<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    key: RetainedSurfaceKey,
    pos: Point,
    modifiers: Modifiers,
    emit_move: bool,
) {
    let Some(surf) = rt.reconciler.surfaces.get_mut(&key) else {
        return;
    };
    let move_dispatch = surf.dispatch_pointer_move_at(pos, modifiers, emit_move);
    if let Some(tr) = move_dispatch.hover {
        if let Some(msg) = tr.on_leave {
            push_message(&rt.inbox, msg);
        }
        if let Some(msg) = tr.on_hover {
            push_message(&rt.inbox, msg);
        }
        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        if let Some((sound, vol)) = tr.on_hover_sound {
            rt.player.play_with_volume(sound, vol);
        }
    }
    if let Some(msg) = move_dispatch.on_node_move {
        push_message(&rt.inbox, msg);
    }
    if let Some(msg) = move_dispatch.on_surface_move {
        push_message(&rt.inbox, msg);
    }
}

fn dispatch_pointer_leave<M: Clone + 'static>(rt: &mut LoopRuntime<M>, key: RetainedSurfaceKey) {
    if let Some(surf) = rt.reconciler.surfaces.get_mut(&key)
        && let Some(tr) = surf.update_hover(None)
        && let Some(on_leave) = tr.on_leave
    {
        push_message(&rt.inbox, on_leave);
    }
}

fn dispatch_pointer_button<M: Clone + 'static>(
    rt: &LoopRuntime<M>,
    key: RetainedSurfaceKey,
    pos: Point,
    button: MouseButton,
    pressed: bool,
    modifiers: Modifiers,
) {
    let Some(surf) = rt.reconciler.surfaces.get(&key) else {
        return;
    };
    let dispatch = if pressed {
        surf.dispatch_pointer_down_at(pos, button, modifiers)
    } else {
        surf.dispatch_pointer_up_at(pos, button, modifiers)
    };
    enqueue_pointer_dispatch(rt, dispatch);
}

fn dispatch_touch_tap<M: Clone + 'static>(
    rt: &LoopRuntime<M>,
    key: RetainedSurfaceKey,
    pos: Point,
) {
    if let Some(surf) = rt.reconciler.surfaces.get(&key) {
        enqueue_pointer_dispatch(rt, surf.dispatch_click_at(pos));
    }
}

fn dispatch_scroll<M: Clone + 'static>(
    rt: &LoopRuntime<M>,
    key: RetainedSurfaceKey,
    event: ScrollEvent,
) {
    if let Some(surf) = rt.reconciler.surfaces.get(&key)
        && let Some(msg) = surf.dispatch_scroll_at(event)
    {
        push_message(&rt.inbox, msg);
    }
}

fn dispatch_keyboard_key<M: Clone + 'static>(
    rt: &LoopRuntime<M>,
    key: RetainedSurfaceKey,
    repeat: bool,
    logical_key: Key,
    modifiers: Modifiers,
    text: Option<String>,
) {
    if let Some(surf) = rt.reconciler.surfaces.get(&key)
        && let Some(ref on_key) = surf.on_key
    {
        let kev = KeyEvent {
            key: logical_key,
            modifiers,
            repeat,
            utf8: text.map(Into::into),
        };
        if let Some(msg) = on_key.call(kev) {
            push_message(&rt.inbox, msg);
        }
    }
}

/// Phase 2: drain Wayland events (outputs, configures, scales, frame/buffer releases, input).
pub(crate) fn drain_wayland_events<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    wl: &mut WaylandState,
) {
    while let Some(ev) = wl.events.pop_front() {
        match ev {
            WaylandEvent::OutputsChanged => {
                rt.capabilities = wl.capabilities();
                rt.capabilities.set(
                    crate::capabilities::Capability::GpuRenderer,
                    rt.renderer.is_gpu_active(),
                );
                #[cfg(feature = "global-shortcuts")]
                rt.capabilities.set(
                    crate::capabilities::Capability::GlobalShortcuts,
                    rt.dbus_client.has_global_shortcuts_portal(),
                );
                #[cfg(feature = "accessibility")]
                rt.capabilities.set(
                    crate::capabilities::Capability::Accessibility,
                    crate::accessibility::atspi_runtime_available(),
                );
                rt.needs_view_sync = true;
            }
            WaylandEvent::SurfaceConfigured { key, size } => {
                rt.configured_sizes.insert(key, size);
                rt.needs_view_sync = true;
            }
            WaylandEvent::SurfaceScaleChanged { key, scale } => {
                rt.surface_scales.insert(key, scale);
                rt.needs_view_sync = true;
            }
            WaylandEvent::SurfaceClosed { key } => {
                let is_popup = rt
                    .reconciler
                    .surfaces
                    .get(&key)
                    .is_some_and(|s| s.spec.kind == crate::surface::SurfaceKind::Popup);
                if is_popup {
                    wl.destroy_surface(key);
                    rt.renderer.drop_surface_target(key);
                    rt.configured_sizes.remove(&key);
                    rt.surface_scales.remove(&key);
                }
                if let Some(surf) = rt.reconciler.surfaces.get(&key)
                    && let Some(msg) = &surf.on_close
                {
                    push_message(&rt.inbox, msg.clone());
                } else if !is_popup {
                    rt.quit_requested = true;
                }
            }
            WaylandEvent::ActivationToken { request_id, token } => {
                if let Some(cb) = rt.pending_activation_callbacks.remove(&request_id)
                    && let Ok(msg) = cb(Ok(token)).downcast::<M>()
                {
                    push_message(&rt.inbox, *msg);
                }
            }
            WaylandEvent::SurfaceFrame { key } => {
                let (_, _, anim_msgs) = rt.reconciler.tick_animations(Instant::now());
                if !anim_msgs.is_empty() {
                    rt.inbox.borrow_mut().messages.extend(anim_msgs);
                }
                if let Some(s) = rt.reconciler.surfaces.get(&key)
                    && s.needs_redraw
                {
                    wl.notify_surface_damage(key, &s.dirty_buffer_rects(), s.full_redraw);
                }
            }
            WaylandEvent::BufferReleased { key } => {
                if let Some(s) = rt.reconciler.surfaces.get_mut(&key)
                    && s.needs_redraw
                {
                    wl.notify_surface_damage(key, &s.dirty_buffer_rects(), s.full_redraw);
                }
            }
            WaylandEvent::PointerEnter {
                key,
                x,
                y,
                modifiers,
            } => {
                dispatch_pointer_hover_and_move(
                    rt,
                    key,
                    Point::new(LogicalPx(x), LogicalPx(y)),
                    modifiers,
                    false,
                );
            }
            WaylandEvent::PointerMotion {
                key,
                x,
                y,
                modifiers,
            } => {
                dispatch_pointer_hover_and_move(
                    rt,
                    key,
                    Point::new(LogicalPx(x), LogicalPx(y)),
                    modifiers,
                    true,
                );
            }
            WaylandEvent::PointerLeave { key } => dispatch_pointer_leave(rt, key),
            WaylandEvent::PointerButton {
                key,
                x,
                y,
                button,
                pressed,
                modifiers,
            } => {
                dispatch_pointer_button(
                    rt,
                    key,
                    Point::new(LogicalPx(x), LogicalPx(y)),
                    button,
                    pressed,
                    modifiers,
                );
            }
            WaylandEvent::PointerScroll {
                key,
                x,
                y,
                dx,
                dy,
                discrete_x,
                discrete_y,
                modifiers,
            } => {
                dispatch_scroll(
                    rt,
                    key,
                    ScrollEvent {
                        position: Point::new(LogicalPx(x), LogicalPx(y)),
                        local_position: Point::ZERO,
                        delta_x: dx,
                        delta_y: dy,
                        discrete_x,
                        discrete_y,
                        modifiers,
                    },
                );
            }
            WaylandEvent::TouchDown { key, id, x, y } => {
                let pos = Point::new(LogicalPx(x), LogicalPx(y));
                rt.touch_positions.insert(id, pos);
                dispatch_touch_tap(rt, key, pos);
            }
            WaylandEvent::TouchMotion { id, x, y } => {
                rt.touch_positions
                    .insert(id, Point::new(LogicalPx(x), LogicalPx(y)));
            }
            WaylandEvent::TouchUp { id } => {
                rt.touch_positions.remove(&id);
            }
            WaylandEvent::KeyboardKey {
                key,
                pressed,
                repeat,
                logical_key,
                modifiers,
                text,
            } => {
                if pressed {
                    dispatch_keyboard_key(rt, key, repeat, logical_key, modifiers, text);
                }
            }
            WaylandEvent::KeyboardLayoutChanged(info) => {
                let mut guard = rt.inbox.borrow_mut();
                for map in &rt.sub_mgr.keyboard_layout_subs {
                    guard.messages.push_back(map.call(info.clone()));
                }
            }
            #[cfg(feature = "idle-notify")]
            WaylandEvent::Idle {
                timeout,
                input_only,
                idle,
            } => {
                let ev = if idle {
                    crate::idle::IdleEvent::Idled(timeout)
                } else {
                    crate::idle::IdleEvent::Resumed(timeout)
                };
                let mut guard = rt.inbox.borrow_mut();
                for (sub_t, sub_inp, map) in &rt.sub_mgr.idle_subs {
                    if *sub_t == timeout && *sub_inp == input_only {
                        guard.messages.push_back(map.call(ev));
                    }
                }
            }
            #[cfg(feature = "clipboard")]
            WaylandEvent::Clipboard(clip_ev) => {
                dispatch_clipboard_event(rt, clip_ev);
            }
            #[cfg(feature = "ime")]
            WaylandEvent::Ime { key, event } => {
                if matches!(event, crate::input::ImeEvent::Enter)
                    && let Some(surf) = rt.reconciler.surfaces.get(&key)
                {
                    wl.sync_ime_for_surface(key, &surf.spec.ime);
                }
                if let Some(surf) = rt.reconciler.surfaces.get(&key)
                    && let Some(ref on_ime) = surf.on_ime
                    && let Some(msg) = on_ime.call(event)
                {
                    push_message(&rt.inbox, msg);
                }
            }
        }
    }

    #[cfg(feature = "clipboard")]
    for clip_ev in wl.clipboard.drain_async_events() {
        dispatch_clipboard_event(rt, clip_ev);
    }

    #[cfg(feature = "accessibility")]
    if rt.last_a11y_focused_surface != wl.focused_keyboard_surface {
        rt.last_a11y_focused_surface = wl.focused_keyboard_surface;
        rt.a11y_bridge
            .sync(&rt.reconciler.surfaces, wl.focused_keyboard_surface);
    }
}

#[cfg(feature = "clipboard")]
fn dispatch_clipboard_event<M: Clone + 'static>(
    rt: &LoopRuntime<M>,
    clip_ev: crate::clipboard::ClipboardEvent,
) {
    let mut guard = rt.inbox.borrow_mut();
    for (filter_sel, map) in &rt.sub_mgr.clipboard_subs {
        let matches = match (filter_sel, &clip_ev) {
            (None, _) => true,
            (Some(_), crate::clipboard::ClipboardEvent::Error(_)) => true,
            (Some(expected), crate::clipboard::ClipboardEvent::Changed(snap)) => {
                *expected == snap.selection
            }
        };
        if matches {
            guard.messages.push_back(map.call(clip_ev.clone()));
        }
    }
}
