//! Wayland `zwp_pointer_gestures_v1` multi-finger touchpad gesture integration
//! (swipe, pinch/rotate, and hold).

use std::collections::HashMap;

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_pointer;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_gestures::zv1::client::{
    zwp_pointer_gesture_hold_v1::{self, ZwpPointerGestureHoldV1},
    zwp_pointer_gesture_pinch_v1::{self, ZwpPointerGesturePinchV1},
    zwp_pointer_gesture_swipe_v1::{self, ZwpPointerGestureSwipeV1},
    zwp_pointer_gestures_v1::{self, ZwpPointerGesturesV1},
};

use crate::geometry::Point;
use crate::input::{GestureEvent, GestureKind, GesturePhase};
use crate::reconcile::RetainedSurfaceKey;
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::WaylandEvent;

#[derive(Debug, Clone, Copy)]
struct ActiveGestureTarget {
    key: RetainedSurfaceKey,
    fingers: u32,
}

struct PointerGestureObjects {
    swipe: ZwpPointerGestureSwipeV1,
    pinch: ZwpPointerGesturePinchV1,
    hold: Option<ZwpPointerGestureHoldV1>,
    active_swipe: Option<ActiveGestureTarget>,
    active_pinch: Option<ActiveGestureTarget>,
    active_hold: Option<ActiveGestureTarget>,
}

impl Drop for PointerGestureObjects {
    fn drop(&mut self) {
        self.swipe.destroy();
        self.pinch.destroy();
        if let Some(hold) = self.hold.take() {
            hold.destroy();
        }
    }
}

/// Manages the optional `zwp_pointer_gestures_v1` global and per-pointer swipe/pinch/hold proxies.
#[derive(Default)]
pub(crate) struct WaylandPointerGesturesState {
    manager: Option<ZwpPointerGesturesV1>,
    pointers: HashMap<ObjectId, PointerGestureObjects>,
}

impl Drop for WaylandPointerGesturesState {
    fn drop(&mut self) {
        self.pointers.clear();
        if let Some(mgr) = self.manager.take()
            && mgr.version() >= 2
        {
            mgr.release();
        }
    }
}

impl WaylandPointerGesturesState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<ZwpPointerGesturesV1, _, _>(qh, 1..=3, ())
            .ok();
        Self {
            manager,
            pointers: HashMap::new(),
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn add_pointer(
        &mut self,
        pointer: &wl_pointer::WlPointer,
        qh: &QueueHandle<WaylandState>,
    ) {
        let Some(ref mgr) = self.manager else {
            return;
        };
        let pid = pointer.id();
        if self.pointers.contains_key(&pid) {
            return;
        }
        let swipe = mgr.get_swipe_gesture(pointer, qh, pid.clone());
        let pinch = mgr.get_pinch_gesture(pointer, qh, pid.clone());
        let hold = if mgr.version() >= 3 {
            Some(mgr.get_hold_gesture(pointer, qh, pid.clone()))
        } else {
            None
        };
        self.pointers.insert(
            pid,
            PointerGestureObjects {
                swipe,
                pinch,
                hold,
                active_swipe: None,
                active_pinch: None,
                active_hold: None,
            },
        );
    }

    pub(crate) fn clear_all_pointers(&mut self) {
        self.pointers.clear();
    }

    pub(crate) fn clear_surface(&mut self, key: RetainedSurfaceKey) {
        for objs in self.pointers.values_mut() {
            if objs.active_swipe.is_some_and(|t| t.key == key) {
                objs.active_swipe = None;
            }
            if objs.active_pinch.is_some_and(|t| t.key == key) {
                objs.active_pinch = None;
            }
            if objs.active_hold.is_some_and(|t| t.key == key) {
                objs.active_hold = None;
            }
        }
    }
}

fn push_gesture(
    state: &mut WaylandState,
    key: RetainedSurfaceKey,
    kind: GestureKind,
    phase: GesturePhase,
    fingers: u32,
) {
    let pos = state
        .last_pointer_pos
        .get(&key)
        .copied()
        .unwrap_or(Point::ZERO);
    state.events.push_back(WaylandEvent::Gesture {
        key,
        event: GestureEvent {
            kind,
            phase,
            fingers,
            position: pos,
            local_position: pos,
            modifiers: state.modifiers,
        },
    });
}

impl Dispatch<ZwpPointerGesturesV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpPointerGesturesV1,
        _event: zwp_pointer_gestures_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpPointerGestureSwipeV1, ObjectId> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ZwpPointerGestureSwipeV1,
        event: zwp_pointer_gesture_swipe_v1::Event,
        pointer_id: &ObjectId,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_pointer_gesture_swipe_v1::Event::Begin {
                surface, fingers, ..
            } => {
                if let Some(&key) = state.surface_by_obj.get(&surface.id()) {
                    if let Some(objs) = state.pointer_gestures.pointers.get_mut(pointer_id) {
                        objs.active_swipe = Some(ActiveGestureTarget { key, fingers });
                    }
                    push_gesture(
                        state,
                        key,
                        GestureKind::Swipe { dx: 0.0, dy: 0.0 },
                        GesturePhase::Begin,
                        fingers,
                    );
                }
            }
            zwp_pointer_gesture_swipe_v1::Event::Update { dx, dy, .. } => {
                let active = state
                    .pointer_gestures
                    .pointers
                    .get(pointer_id)
                    .and_then(|o| o.active_swipe);
                if let Some(target) = active {
                    push_gesture(
                        state,
                        target.key,
                        GestureKind::Swipe {
                            dx: dx as f32,
                            dy: dy as f32,
                        },
                        GesturePhase::Update,
                        target.fingers,
                    );
                }
            }
            zwp_pointer_gesture_swipe_v1::Event::End { cancelled, .. } => {
                let active = state
                    .pointer_gestures
                    .pointers
                    .get_mut(pointer_id)
                    .and_then(|o| o.active_swipe.take());
                if let Some(target) = active {
                    let phase = if cancelled != 0 {
                        GesturePhase::Cancelled
                    } else {
                        GesturePhase::End
                    };
                    push_gesture(
                        state,
                        target.key,
                        GestureKind::Swipe { dx: 0.0, dy: 0.0 },
                        phase,
                        target.fingers,
                    );
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpPointerGesturePinchV1, ObjectId> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ZwpPointerGesturePinchV1,
        event: zwp_pointer_gesture_pinch_v1::Event,
        pointer_id: &ObjectId,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_pointer_gesture_pinch_v1::Event::Begin {
                surface, fingers, ..
            } => {
                if let Some(&key) = state.surface_by_obj.get(&surface.id()) {
                    if let Some(objs) = state.pointer_gestures.pointers.get_mut(pointer_id) {
                        objs.active_pinch = Some(ActiveGestureTarget { key, fingers });
                    }
                    push_gesture(
                        state,
                        key,
                        GestureKind::Pinch {
                            dx: 0.0,
                            dy: 0.0,
                            scale: 1.0,
                            rotation: 0.0,
                        },
                        GesturePhase::Begin,
                        fingers,
                    );
                }
            }
            zwp_pointer_gesture_pinch_v1::Event::Update {
                dx,
                dy,
                scale,
                rotation,
                ..
            } => {
                let active = state
                    .pointer_gestures
                    .pointers
                    .get(pointer_id)
                    .and_then(|o| o.active_pinch);
                if let Some(target) = active {
                    push_gesture(
                        state,
                        target.key,
                        GestureKind::Pinch {
                            dx: dx as f32,
                            dy: dy as f32,
                            scale: scale as f32,
                            rotation: rotation as f32,
                        },
                        GesturePhase::Update,
                        target.fingers,
                    );
                }
            }
            zwp_pointer_gesture_pinch_v1::Event::End { cancelled, .. } => {
                let active = state
                    .pointer_gestures
                    .pointers
                    .get_mut(pointer_id)
                    .and_then(|o| o.active_pinch.take());
                if let Some(target) = active {
                    let phase = if cancelled != 0 {
                        GesturePhase::Cancelled
                    } else {
                        GesturePhase::End
                    };
                    push_gesture(
                        state,
                        target.key,
                        GestureKind::Pinch {
                            dx: 0.0,
                            dy: 0.0,
                            scale: 1.0,
                            rotation: 0.0,
                        },
                        phase,
                        target.fingers,
                    );
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpPointerGestureHoldV1, ObjectId> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ZwpPointerGestureHoldV1,
        event: zwp_pointer_gesture_hold_v1::Event,
        pointer_id: &ObjectId,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_pointer_gesture_hold_v1::Event::Begin {
                surface, fingers, ..
            } => {
                if let Some(&key) = state.surface_by_obj.get(&surface.id()) {
                    if let Some(objs) = state.pointer_gestures.pointers.get_mut(pointer_id) {
                        objs.active_hold = Some(ActiveGestureTarget { key, fingers });
                    }
                    push_gesture(state, key, GestureKind::Hold, GesturePhase::Begin, fingers);
                }
            }
            zwp_pointer_gesture_hold_v1::Event::End { cancelled, .. } => {
                let active = state
                    .pointer_gestures
                    .pointers
                    .get_mut(pointer_id)
                    .and_then(|o| o.active_hold.take());
                if let Some(target) = active {
                    let phase = if cancelled != 0 {
                        GesturePhase::Cancelled
                    } else {
                        GesturePhase::End
                    };
                    push_gesture(state, target.key, GestureKind::Hold, phase, target.fingers);
                }
            }
            _ => {}
        }
    }
}
