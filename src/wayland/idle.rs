//! Wayland `ext-idle-notify-v1` protocol integration.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_seat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
};

use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::WaylandEvent;

/// User data attached to each `ext_idle_notification_v1` proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct IdleNotificationData {
    timeout: Duration,
    input_only: bool,
}

/// Manages the optional `ext_idle_notifier_v1` global and active idle notification objects.
#[derive(Default)]
pub(crate) struct WaylandIdleState {
    notifier: Option<ExtIdleNotifierV1>,
    bound_seat_id: Option<ObjectId>,
    notifications: HashMap<(Duration, bool), ExtIdleNotificationV1>,
}

impl WaylandIdleState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let notifier = globals.bind::<ExtIdleNotifierV1, _, _>(qh, 1..=2, ()).ok();
        Self {
            notifier,
            bound_seat_id: None,
            notifications: HashMap::new(),
        }
    }

    pub(crate) fn is_supported(&self) -> bool {
        self.notifier.is_some()
    }

    /// Reconciles active `ext_idle_notification_v1` objects with the desired `(timeout, input_only)` set.
    pub(crate) fn sync(
        &mut self,
        seat: Option<&wl_seat::WlSeat>,
        desired: &[(Duration, bool)],
        qh: &QueueHandle<WaylandState>,
    ) {
        let seat_id = seat.map(Proxy::id);
        if seat_id != self.bound_seat_id {
            for (_, notif) in self.notifications.drain() {
                notif.destroy();
            }
            self.bound_seat_id = seat_id;
        }

        let desired_set: HashSet<(Duration, bool)> = desired.iter().copied().collect();

        let removed: Vec<(Duration, bool)> = self
            .notifications
            .keys()
            .filter(|k| !desired_set.contains(k))
            .copied()
            .collect();
        for key in removed {
            if let Some(notif) = self.notifications.remove(&key) {
                notif.destroy();
            }
        }

        let (Some(notifier), Some(seat)) = (&self.notifier, seat) else {
            return;
        };

        for (timeout, input_only) in desired_set {
            if self.notifications.contains_key(&(timeout, input_only)) {
                continue;
            }
            let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
            let data = IdleNotificationData {
                timeout,
                input_only,
            };
            let notif = if input_only && notifier.version() >= 2 {
                notifier.get_input_idle_notification(timeout_ms, seat, qh, data)
            } else {
                notifier.get_idle_notification(timeout_ms, seat, qh, data)
            };
            self.notifications.insert((timeout, input_only), notif);
        }
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtIdleNotifierV1,
        _event: ext_idle_notifier_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, IdleNotificationData> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        data: &IdleNotificationData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let idle = match event {
            ext_idle_notification_v1::Event::Idled => true,
            ext_idle_notification_v1::Event::Resumed => false,
            _ => return,
        };
        state.events.push_back(WaylandEvent::Idle {
            timeout: data.timeout,
            input_only: data.input_only,
            idle,
        });
    }
}
