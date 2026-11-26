//! Wayland `ext_session_lock_v1` native lockscreen protocol backend.

use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_output, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::{self, ExtSessionLockManagerV1},
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};

use crate::capabilities::Capability;
use crate::error::{Result, WaywiError};
use crate::geometry::{LogicalPx, LogicalSize};
use crate::reconcile::RetainedSurfaceKey;
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::WaylandEvent;
use crate::wayland_ext::SessionLockEvent;

/// Wrapper holding both the `wl_surface` and `ext_session_lock_surface_v1` in safe drop order
/// (destroying `ext_session_lock_surface_v1` before `wl_surface`).
pub(crate) struct SessionLockRole {
    lock_surface: Option<ExtSessionLockSurfaceV1>,
    wl_surface: wl_surface::WlSurface,
    pending_configure_serial: Option<u32>,
}

impl SessionLockRole {
    pub(crate) fn wl_surface(&self) -> &wl_surface::WlSurface {
        &self.wl_surface
    }

    /// Sends `ack_configure` for the latest pending compositor configure immediately before
    /// attaching and committing the rendered buffer matching the configured dimensions.
    pub(crate) fn ack_pending_configure(&mut self) {
        if let (Some(serial), Some(ls)) = (
            self.pending_configure_serial.take(),
            self.lock_surface.as_ref(),
        ) {
            ls.ack_configure(serial);
        }
    }
}

impl Drop for SessionLockRole {
    fn drop(&mut self) {
        if let Some(ls) = self.lock_surface.take() {
            ls.destroy();
        }
        self.wl_surface.destroy();
    }
}

/// Manages the singleton `ext_session_lock_manager_v1` and active `ext_session_lock_v1` session.
pub(crate) struct WaylandSessionLockState {
    manager: Option<ExtSessionLockManagerV1>,
    lock: Option<ExtSessionLockV1>,
    locked: bool,
    finished: bool,
}

impl Drop for WaylandSessionLockState {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take() {
            if self.locked && !self.finished {
                lock.unlock_and_destroy();
            } else {
                lock.destroy();
            }
        }
        if let Some(mgr) = self.manager.take() {
            mgr.destroy();
        }
    }
}

impl WaylandSessionLockState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<ExtSessionLockManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        Self {
            manager,
            lock: None,
            locked: false,
            finished: false,
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    /// Creates an `ext_session_lock_surface_v1` bound to `output`, acquiring the session lock
    /// automatically if not already active.
    pub(crate) fn create_lock_surface(
        &mut self,
        qh: &QueueHandle<WaylandState>,
        key: RetainedSurfaceKey,
        wl_surface: wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) -> Result<SessionLockRole> {
        let mgr = self
            .manager
            .as_ref()
            .ok_or(WaywiError::UnsupportedCapability(Capability::SessionLock))?;
        if self.lock.is_none() {
            self.locked = false;
            self.finished = false;
            self.lock = Some(mgr.lock(qh, ()));
        }
        let lock = self.lock.as_ref().expect("session lock acquired");
        let lock_surface = lock.get_lock_surface(&wl_surface, output, qh, key);
        Ok(SessionLockRole {
            lock_surface: Some(lock_surface),
            wl_surface,
            pending_configure_serial: None,
        })
    }

    /// Unlocks and destroys the active `ext_session_lock_v1` once all lock surfaces are removed.
    pub(crate) fn maybe_unlock(&mut self, has_any_lock_surface: bool) {
        if has_any_lock_surface {
            return;
        }
        if let Some(lock) = self.lock.take() {
            if self.locked && !self.finished {
                lock.unlock_and_destroy();
            } else {
                lock.destroy();
            }
            self.locked = false;
            self.finished = false;
        }
    }
}

impl Dispatch<ExtSessionLockManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtSessionLockManagerV1,
        _event: ext_session_lock_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtSessionLockV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => {
                state.session_lock.locked = true;
                state
                    .events
                    .push_back(WaylandEvent::SessionLock(SessionLockEvent::Locked));
            }
            ext_session_lock_v1::Event::Finished => {
                state.session_lock.finished = true;
                if let Some(lock) = state.session_lock.lock.take() {
                    lock.destroy();
                }
                state.session_lock.locked = false;
                state
                    .events
                    .push_back(WaylandEvent::SessionLock(SessionLockEvent::Finished));
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, RetainedSurfaceKey> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        key: &RetainedSurfaceKey,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
            && let Some(surf) = state.surfaces.get_mut(key)
        {
            if let crate::wayland::surface_state::SurfaceRole::SessionLock(ref mut sl) = surf.role {
                sl.pending_configure_serial = Some(serial);
            }
            let w = if width > 0 {
                width as f32
            } else {
                surf.spec.logical_size.width.0.max(1.0)
            };
            let h = if height > 0 {
                height as f32
            } else {
                surf.spec.logical_size.height.0.max(1.0)
            };
            let size = LogicalSize::new(LogicalPx(w), LogicalPx(h));
            surf.configured = true;
            surf.configured_size = Some(size);
            surf.frame_pending = false;
            // Always schedule a present so `ack_configure(serial)` is sent atomically with a
            // buffer matching the configured `(width, height)`.
            surf.pending_redraw = true;
            state
                .events
                .push_back(WaylandEvent::SurfaceConfigured { key: *key, size });
        }
    }
}
