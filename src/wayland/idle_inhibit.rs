//! Wayland `zwp_idle_inhibit_manager_v1` per-surface idle inhibition backend.

use std::collections::HashMap;

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_surface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::{self, ZwpIdleInhibitManagerV1},
    zwp_idle_inhibitor_v1::{self, ZwpIdleInhibitorV1},
};

use crate::wayland::connection::WaylandState;

/// Manages declarative `zwp_idle_inhibitor_v1` objects tied to live `wl_surface`s.
pub(crate) struct WaylandIdleInhibitState {
    manager: Option<ZwpIdleInhibitManagerV1>,
    inhibitors: HashMap<ObjectId, ZwpIdleInhibitorV1>,
}

impl Drop for WaylandIdleInhibitState {
    fn drop(&mut self) {
        for (_, inh) in self.inhibitors.drain() {
            inh.destroy();
        }
        if let Some(mgr) = self.manager.take() {
            mgr.destroy();
        }
    }
}

impl WaylandIdleInhibitState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<ZwpIdleInhibitManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        Self {
            manager,
            inhibitors: HashMap::new(),
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    /// Synchronizes idle inhibition state for `surface` (idempotent; zero wire traffic when unchanged).
    pub(crate) fn sync_surface(
        &mut self,
        surface: &wl_surface::WlSurface,
        inhibit: bool,
        qh: &QueueHandle<WaylandState>,
    ) {
        let id = surface.id();
        if inhibit {
            if !self.inhibitors.contains_key(&id)
                && let Some(ref mgr) = self.manager
            {
                let inh = mgr.create_inhibitor(surface, qh, ());
                self.inhibitors.insert(id, inh);
            }
        } else if let Some(inh) = self.inhibitors.remove(&id) {
            inh.destroy();
        }
    }

    pub(crate) fn clear_surface(&mut self, wl_id: &ObjectId) {
        if let Some(inh) = self.inhibitors.remove(wl_id) {
            inh.destroy();
        }
    }

    pub(crate) fn clear_all(&mut self) {
        for (_, inh) in self.inhibitors.drain() {
            inh.destroy();
        }
    }
}

impl Dispatch<ZwpIdleInhibitManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpIdleInhibitManagerV1,
        _event: zwp_idle_inhibit_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpIdleInhibitorV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpIdleInhibitorV1,
        _event: zwp_idle_inhibitor_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
