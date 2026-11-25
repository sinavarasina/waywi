use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use crate::app::subscription_mgr::{LoopInbox, fanout_event};
use crate::geometry::{LogicalPx, Point};
use crate::input::{Key, KeyEvent, Modifiers, MouseButton, ScrollEvent};
use crate::reconcile::RetainedSurfaceKey;
use crate::wayland::{WaylandEvent, WaylandState};

use super::LoopRuntime;

fn push_message<M: Clone + 'static>(inbox: &Rc<RefCell<LoopInbox<M>>>, msg: M) {
    inbox.borrow_mut().messages.push_back(msg);
}

fn dispatch_pointer_hover_and_move<M: Clone + 'static>(
    rt: &mut LoopRuntime<M>,
    key: RetainedSurfaceKey,
    pos: Point,
    modifiers: Modifiers,
    emit_move: bool,
) {
    let mut guard = rt.inbox.borrow_mut();
    rt.core
        .dispatch_pointer_move(key, pos, modifiers, emit_move, &mut guard.messages);
}

fn dispatch_pointer_leave<M: Clone + 'static>(rt: &mut LoopRuntime<M>, key: RetainedSurfaceKey) {
    if let Some(surf) = rt.core.reconciler.surfaces.get_mut(&key)
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
    let Some(surf) = rt.core.reconciler.surfaces.get(&key) else {
        return;
    };
    let dispatch = if pressed {
        surf.dispatch_pointer_down_at(pos, button, modifiers)
    } else {
        surf.dispatch_pointer_up_at(pos, button, modifiers)
    };
    rt.core
        .dispatch_pointer_result(dispatch, &mut rt.inbox.borrow_mut().messages);
}

fn dispatch_touch_tap<M: Clone + 'static>(
    rt: &LoopRuntime<M>,
    key: RetainedSurfaceKey,
    pos: Point,
) {
    if let Some(surf) = rt.core.reconciler.surfaces.get(&key) {
        let dispatch = surf.dispatch_click_at(pos);
        rt.core
            .dispatch_pointer_result(dispatch, &mut rt.inbox.borrow_mut().messages);
    }
}

fn dispatch_scroll<M: Clone + 'static>(
    rt: &LoopRuntime<M>,
    key: RetainedSurfaceKey,
    event: ScrollEvent,
) {
    if let Some(surf) = rt.core.reconciler.surfaces.get(&key)
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
    if let Some(surf) = rt.core.reconciler.surfaces.get(&key)
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
                wl.refresh_outputs();
                wl.clear_frame_pending_all();
                wl.refresh_wayland_capabilities(&mut rt.core.capabilities);
                rt.core.memo_store.invalidate_all();
                rt.needs_view_sync = true;
            }
            WaylandEvent::SurfaceConfigured { key, size } => {
                if rt.core.configured_sizes.insert(key, size) != Some(size) {
                    rt.core.memo_store.invalidate_surface(key.surface_key.0);
                }
                rt.needs_view_sync = true;
            }
            WaylandEvent::SurfaceScaleChanged { key, scale } => {
                if rt.core.surface_scales.insert(key, scale) != Some(scale) {
                    rt.core.memo_store.invalidate_surface(key.surface_key.0);
                }
                rt.needs_view_sync = true;
            }
            WaylandEvent::WindowCloseRequested { key } => {
                if let Some(surf) = rt.core.reconciler.surfaces.get(&key)
                    && let Some(msg) = &surf.on_close
                {
                    push_message(&rt.inbox, msg.clone());
                } else {
                    rt.core.quit_requested = true;
                }
            }
            WaylandEvent::PopupDismissed { key } => {
                if let Some(msg) = super::present::teardown_surface_with_popups(rt, wl, key, false)
                {
                    push_message(&rt.inbox, msg);
                }
            }
            WaylandEvent::LayerClosed { key } => {
                rt.core.memo_store.invalidate_surface(key.surface_key.0);
                if let Some(msg) = super::present::teardown_surface_with_popups(rt, wl, key, false)
                {
                    push_message(&rt.inbox, msg);
                }
            }
            WaylandEvent::ActivationToken { request_id, token } => {
                if let Some(cb) = rt.pending_activation_callbacks.remove(&request_id) {
                    push_message(&rt.inbox, cb(Ok(token)));
                }
            }
            WaylandEvent::SurfaceFrame { key } => {
                let (_, _, anim_msgs) = rt.core.reconciler.tick_animations(Instant::now());
                if !anim_msgs.is_empty() {
                    rt.inbox.borrow_mut().messages.extend(anim_msgs);
                }
                if let Some(s) = rt.core.reconciler.surfaces.get(&key)
                    && s.needs_redraw
                {
                    wl.notify_surface_damage(key, &s.dirty_buffer_rects(), s.full_redraw);
                }
            }
            WaylandEvent::BufferReleased { key } => {
                if let Some(s) = rt.core.reconciler.surfaces.get_mut(&key)
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
                fanout_event(
                    info,
                    &rt.sub_mgr.keyboard_layout_subs,
                    &mut rt.inbox.borrow_mut().messages,
                );
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
                    && let Some(surf) = rt.core.reconciler.surfaces.get(&key)
                {
                    wl.sync_ime_for_surface(key, &surf.spec.ime);
                }
                if let Some(surf) = rt.core.reconciler.surfaces.get(&key)
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
            .sync(&rt.core.reconciler.surfaces, wl.focused_keyboard_surface);
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
