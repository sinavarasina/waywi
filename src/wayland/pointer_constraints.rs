//! Wayland `zwp_pointer_constraints_v1` (lock/confine) and `zwp_relative_pointer_v1` backend.

use std::collections::HashMap;

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_pointer, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_confined_pointer_v1::{self, ZwpConfinedPointerV1},
    zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{self, Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::{self, ZwpRelativePointerManagerV1},
    zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
};

use crate::input::{PointerConstraintMode, RelativePointerEvent};
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::WaylandEvent;

enum ActiveConstraint {
    Locked(ZwpLockedPointerV1),
    Confined(ZwpConfinedPointerV1),
}

impl ActiveConstraint {
    fn mode(&self) -> PointerConstraintMode {
        match self {
            Self::Locked(_) => PointerConstraintMode::Locked,
            Self::Confined(_) => PointerConstraintMode::Confined,
        }
    }

    fn destroy(self) {
        match self {
            Self::Locked(p) => p.destroy(),
            Self::Confined(p) => p.destroy(),
        }
    }
}

/// Manages `zwp_pointer_constraints_v1` per-surface locks/confinements and
/// `zwp_relative_pointer_v1` relative motion streams.
pub(crate) struct WaylandPointerConstraintsState {
    constraints_mgr: Option<ZwpPointerConstraintsV1>,
    relative_mgr: Option<ZwpRelativePointerManagerV1>,
    relative_pointers: HashMap<ObjectId, ZwpRelativePointerV1>,
    active: HashMap<ObjectId, ActiveConstraint>,
    desired: HashMap<ObjectId, (wl_surface::WlSurface, PointerConstraintMode)>,
}

impl Drop for WaylandPointerConstraintsState {
    fn drop(&mut self) {
        for (_, c) in self.active.drain() {
            c.destroy();
        }
        for (_, rp) in self.relative_pointers.drain() {
            rp.destroy();
        }
        if let Some(m) = self.constraints_mgr.take() {
            m.destroy();
        }
        if let Some(m) = self.relative_mgr.take() {
            m.destroy();
        }
    }
}

impl WaylandPointerConstraintsState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let constraints_mgr = globals
            .bind::<ZwpPointerConstraintsV1, _, _>(qh, 1..=1, ())
            .ok();
        let relative_mgr = globals
            .bind::<ZwpRelativePointerManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        Self {
            constraints_mgr,
            relative_mgr,
            relative_pointers: HashMap::new(),
            active: HashMap::new(),
            desired: HashMap::new(),
        }
    }

    #[must_use]
    pub(crate) fn supports_constraints(&self) -> bool {
        self.constraints_mgr.is_some()
    }

    #[must_use]
    pub(crate) fn supports_relative(&self) -> bool {
        self.relative_mgr.is_some()
    }

    pub(crate) fn add_pointer(
        &mut self,
        pointer: &wl_pointer::WlPointer,
        qh: &QueueHandle<WaylandState>,
    ) {
        if let Some(ref mgr) = self.relative_mgr {
            let pid = pointer.id();
            self.relative_pointers
                .entry(pid)
                .or_insert_with(|| mgr.get_relative_pointer(pointer, qh, ()));
        }
        // Attach any pending surface constraints now that a `wl_pointer` exists.
        if let Some(ref cmgr) = self.constraints_mgr {
            for (wl_id, (surf, mode)) in &self.desired {
                if *mode == PointerConstraintMode::None || self.active.contains_key(wl_id) {
                    continue;
                }
                let next =
                    match mode {
                        PointerConstraintMode::Locked => ActiveConstraint::Locked(
                            cmgr.lock_pointer(surf, pointer, None, Lifetime::Persistent, qh, ()),
                        ),
                        PointerConstraintMode::Confined => ActiveConstraint::Confined(
                            cmgr.confine_pointer(surf, pointer, None, Lifetime::Persistent, qh, ()),
                        ),
                        PointerConstraintMode::None => continue,
                    };
                self.active.insert(wl_id.clone(), next);
            }
        }
    }

    pub(crate) fn clear_all_pointers(&mut self) {
        for (_, c) in self.active.drain() {
            c.destroy();
        }
        for (_, rp) in self.relative_pointers.drain() {
            rp.destroy();
        }
    }

    /// Synchronizes a surface's desired [`PointerConstraintMode`], always destroying any prior
    /// constraint before requesting a new one to uphold `zwp_pointer_constraints_v1` protocol rules.
    pub(crate) fn sync_surface(
        &mut self,
        surface: &wl_surface::WlSurface,
        mode: PointerConstraintMode,
        pointer: Option<&wl_pointer::WlPointer>,
        qh: &QueueHandle<WaylandState>,
    ) {
        let wl_id = surface.id();
        if mode == PointerConstraintMode::None {
            self.desired.remove(&wl_id);
            if let Some(old) = self.active.remove(&wl_id) {
                old.destroy();
            }
            return;
        }

        self.desired.insert(wl_id.clone(), (surface.clone(), mode));

        if self.active.get(&wl_id).is_some_and(|c| c.mode() == mode) {
            return;
        }
        if let Some(old) = self.active.remove(&wl_id) {
            old.destroy();
        }

        let (Some(cmgr), Some(ptr)) = (&self.constraints_mgr, pointer) else {
            return;
        };
        let created =
            match mode {
                PointerConstraintMode::Locked => ActiveConstraint::Locked(cmgr.lock_pointer(
                    surface,
                    ptr,
                    None,
                    Lifetime::Persistent,
                    qh,
                    (),
                )),
                PointerConstraintMode::Confined => ActiveConstraint::Confined(
                    cmgr.confine_pointer(surface, ptr, None, Lifetime::Persistent, qh, ()),
                ),
                PointerConstraintMode::None => return,
            };
        self.active.insert(wl_id, created);
    }

    pub(crate) fn clear_surface(&mut self, wl_id: &ObjectId) {
        self.desired.remove(wl_id);
        if let Some(old) = self.active.remove(wl_id) {
            old.destroy();
        }
    }

    pub(crate) fn clear_all_surfaces(&mut self) {
        self.desired.clear();
        for (_, old) in self.active.drain() {
            old.destroy();
        }
    }

    /// Returns the `wl_surface` object ID currently holding a `Locked` pointer constraint, if any.
    #[must_use]
    pub(crate) fn locked_surface_id(&self) -> Option<&ObjectId> {
        self.active
            .iter()
            .find_map(|(id, c)| matches!(c, ActiveConstraint::Locked(_)).then_some(id))
    }
}

impl Dispatch<ZwpPointerConstraintsV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpPointerConstraintsV1,
        _event: zwp_pointer_constraints_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpLockedPointerV1,
        _event: zwp_locked_pointer_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpConfinedPointerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpConfinedPointerV1,
        _event: zwp_confined_pointer_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpRelativePointerManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpRelativePointerManagerV1,
        _event: zwp_relative_pointer_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            utime_hi,
            utime_lo,
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
        } = event
        {
            let target_key = state.pointer_focus_surface.or_else(|| {
                state
                    .pointer_constraints
                    .locked_surface_id()
                    .and_then(|wl_id| state.surface_by_obj.get(wl_id).copied())
            });
            if let Some(key) = target_key {
                let time_us = ((utime_hi as u64) << 32) | (utime_lo as u64);
                state.events.push_back(WaylandEvent::RelativePointer {
                    key,
                    event: RelativePointerEvent {
                        time_us,
                        dx,
                        dy,
                        dx_unaccel,
                        dy_unaccel,
                    },
                });
            }
        }
    }
}
