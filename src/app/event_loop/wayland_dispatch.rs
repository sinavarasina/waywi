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
) -> Option<crate::input::CursorShape> {
    let mut guard = rt.inbox.borrow_mut();
    rt.core
        .dispatch_pointer_move(key, pos, modifiers, emit_move, &mut guard.messages)
        .1
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
    let mut dispatched_anim_frame = false;
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
                let now = Instant::now();
                let (dirty_surfs, _, anim_msgs) =
                    rt.core.reconciler.tick_animations_for(Some(key), now);
                if !anim_msgs.is_empty() {
                    rt.inbox.borrow_mut().messages.extend(anim_msgs);
                }
                for skey in dirty_surfs {
                    if let Some(s) = rt.core.reconciler.surfaces.get_mut(&skey) {
                        if s.input_region_dirty {
                            wl.apply_input_region(
                                skey,
                                s.spec.input_mode,
                                &s.selective_input_rects,
                            );
                            s.input_region_dirty = false;
                        }
                        if s.needs_redraw {
                            wl.notify_surface_damage(skey, &s.dirty_buffer_rects(), s.full_redraw);
                        }
                    }
                }
                if let Some(s) = rt.core.reconciler.surfaces.get(&key)
                    && s.needs_redraw
                {
                    wl.notify_surface_damage(key, &s.dirty_buffer_rects(), s.full_redraw);
                }
                if !dispatched_anim_frame && !rt.sub_mgr.animation_frame_subs.is_empty() {
                    dispatched_anim_frame = true;
                    fanout_event(
                        now,
                        &rt.sub_mgr.animation_frame_subs,
                        &mut rt.inbox.borrow_mut().messages,
                    );
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
                let pos = Point::new(LogicalPx(x), LogicalPx(y));
                if let Some(shape) = dispatch_pointer_hover_and_move(rt, key, pos, modifiers, false)
                {
                    wl.apply_cursor_shape(shape);
                }
            }
            WaylandEvent::PointerMotion {
                key,
                x,
                y,
                modifiers,
            } => {
                let pos = Point::new(LogicalPx(x), LogicalPx(y));
                if let Some(shape) = dispatch_pointer_hover_and_move(rt, key, pos, modifiers, true)
                {
                    wl.apply_cursor_shape(shape);
                }
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
                source,
                stop_x,
                stop_y,
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
                        source,
                        stop_x,
                        stop_y,
                        modifiers,
                    },
                );
            }
            WaylandEvent::Touch { key, event } => {
                let mut guard = rt.inbox.borrow_mut();
                rt.core.dispatch_touch(key, event, &mut guard.messages);
            }
            #[cfg(feature = "pointer-gestures")]
            WaylandEvent::Gesture { key, event } => {
                let mut guard = rt.inbox.borrow_mut();
                rt.core.dispatch_gesture(key, event, &mut guard.messages);
            }
            #[cfg(feature = "tablet")]
            WaylandEvent::Tablet { key, event } => {
                let mut guard = rt.inbox.borrow_mut();
                rt.core.dispatch_tablet(key, event, &mut guard.messages);
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
            WaylandEvent::Dnd { key, event } => {
                if let Some(surf) = rt.core.reconciler.surfaces.get(&key)
                    && let Some(ref on_dnd) = surf.on_dnd
                    && let Some(msg) = on_dnd.call(event)
                {
                    push_message(&rt.inbox, msg);
                }
            }
            #[cfg(feature = "session-lock")]
            WaylandEvent::SessionLock(sl_ev) => {
                fanout_event(
                    sl_ev,
                    &rt.sub_mgr.session_lock_subs,
                    &mut rt.inbox.borrow_mut().messages,
                );
            }
            #[cfg(feature = "screencopy")]
            WaylandEvent::ScreencopyDone { request_id, result } => {
                if let Some(cb) = rt.pending_screencopy_callbacks.remove(&request_id) {
                    push_message(&rt.inbox, cb(result));
                }
            }
            #[cfg(feature = "pointer-constraints")]
            WaylandEvent::RelativePointer { key, event } => {
                if let Some(surf) = rt.core.reconciler.surfaces.get(&key)
                    && let Some(ref on_rel) = surf.on_relative_pointer
                    && let Some(msg) = on_rel.call(event)
                {
                    push_message(&rt.inbox, msg);
                }
            }
        }
    }

    #[cfg(feature = "foreign-toplevel")]
    if wl.foreign_toplevel.take_dirty() && !rt.sub_mgr.foreign_toplevel_subs.is_empty() {
        let snap = wl.foreign_toplevel.snapshot(&wl.output_handles);
        fanout_event(
            snap,
            &rt.sub_mgr.foreign_toplevel_subs,
            &mut rt.inbox.borrow_mut().messages,
        );
    }

    #[cfg(feature = "ext-workspace")]
    if wl.ext_workspace.take_dirty() && !rt.sub_mgr.ext_workspace_subs.is_empty() {
        let snap = wl.ext_workspace.snapshot(&wl.output_handles);
        fanout_event(
            snap,
            &rt.sub_mgr.ext_workspace_subs,
            &mut rt.inbox.borrow_mut().messages,
        );
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
