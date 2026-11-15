//! XDG Desktop Notifications daemon (`org.freedesktop.Notifications`) support.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::DbusClient;
use super::types::Dbus;

const NOTIFICATIONS_BUS_NAME: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_OBJECT_PATH: &str = "/org/freedesktop/Notifications";

/// Notification urgency level defined by the XDG Desktop Notifications specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NotificationUrgency {
    Low = 0,
    #[default]
    Normal = 1,
    Critical = 2,
}

/// Standard `NotificationClosed` reason codes from the XDG Desktop Notifications specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum NotificationCloseReason {
    Expired = 1,
    Dismissed = 2,
    ClosedByCall = 3,
    Undefined = 4,
}

/// Raw uncompressed pixel payload carried inside `image-data` / `icon_data` hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationImageData {
    pub width: i32,
    pub height: i32,
    pub rowstride: i32,
    pub has_alpha: bool,
    pub bits_per_sample: i32,
    pub channels: i32,
    pub data: Vec<u8>,
}

/// Typed XDG desktop notification received over `org.freedesktop.Notifications.Notify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopNotification {
    pub id: u32,
    pub app_name: String,
    pub replaces_id: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    /// Action `(action_key, display_label)` pairs declared by the sending client.
    pub actions: Vec<(String, String)>,
    pub urgency: NotificationUrgency,
    pub desktop_entry: Option<String>,
    pub image_path: Option<String>,
    pub image_data: Option<NotificationImageData>,
    /// Requested expiration timeout in milliseconds (`-1` = server default, `0` = never expire).
    pub expire_timeout_ms: i32,
    /// Local wall-clock timestamp recorded immediately when the notification was received.
    pub received_at: SystemTime,
}

/// Events emitted by [`Dbus::notifications`] when acting as the session notification server.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationEvent {
    Received(Box<DesktopNotification>),
    ClosedByCaller { id: u32 },
    ServerError(String),
}

impl Dbus {
    /// Registers an `org.freedesktop.Notifications` server on the session D-Bus and streams
    /// incoming desktop notifications.
    pub fn notifications<M: 'static>(
        map: impl Fn(NotificationEvent) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusNotifications {
                key: "dbus:org.freedesktop.Notifications".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Emits `org.freedesktop.Notifications.NotificationClosed(id, reason)` from the active server.
    pub fn close_notification(&self, id: u32, reason: NotificationCloseReason) -> Result<()> {
        let conn = self.active_notifications_conn()?;
        futures_lite::future::block_on(async move {
            let iface = conn
                .object_server()
                .interface::<_, NotificationsIface>(NOTIFICATIONS_OBJECT_PATH)
                .await
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            NotificationsIface::notification_closed(iface.signal_emitter(), id, reason as u32)
                .await
                .map_err(|e| WaywiError::DbusError(e.to_string()))
        })
    }

    /// Emits `org.freedesktop.Notifications.ActionInvoked(id, action_key)` from the active server.
    pub fn invoke_notification_action(&self, id: u32, action_key: &str) -> Result<()> {
        let conn = self.active_notifications_conn()?;
        futures_lite::future::block_on(async move {
            let iface = conn
                .object_server()
                .interface::<_, NotificationsIface>(NOTIFICATIONS_OBJECT_PATH)
                .await
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            NotificationsIface::action_invoked(iface.signal_emitter(), id, action_key)
                .await
                .map_err(|e| WaywiError::DbusError(e.to_string()))
        })
    }

    fn active_notifications_conn(&self) -> Result<zbus::Connection> {
        self.notifications_conn
            .lock()
            .map_err(|_| WaywiError::DbusError("notification connection lock poisoned".into()))?
            .clone()
            .ok_or_else(|| {
                WaywiError::DbusError("org.freedesktop.Notifications server is not running".into())
            })
    }
}

pub(crate) struct NotificationServerWorker {
    stop: super::worker::AsyncStopSignal,
    conn_slot: Arc<Mutex<Option<zbus::Connection>>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl NotificationServerWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<NotificationEvent>,
        conn_slot: Arc<Mutex<Option<zbus::Connection>>>,
    ) -> Self {
        let stop = super::worker::AsyncStopSignal::new();
        let stop_flag = stop.clone();
        let slot_for_thread = Arc::clone(&conn_slot);

        let join = std::thread::Builder::new()
            .name("waywi-dbus-notify".to_owned())
            .spawn(move || {
                let iface = NotificationsIface {
                    next_id: AtomicU32::new(1),
                    sender: sender.clone(),
                };
                let conn_res = futures_lite::future::block_on(async move {
                    let builder = zbus::connection::Builder::session()?
                        .name(NOTIFICATIONS_BUS_NAME)?
                        .serve_at(NOTIFICATIONS_OBJECT_PATH, iface)?;
                    builder.build().await
                });
                let Ok(conn) = conn_res else {
                    if let Err(err) = conn_res {
                        let _ = sender.send(NotificationEvent::ServerError(format!(
                            "failed to claim {NOTIFICATIONS_BUS_NAME}: {err}"
                        )));
                    }
                    return;
                };

                if let Ok(mut guard) = slot_for_thread.lock() {
                    *guard = Some(conn.clone());
                }

                futures_lite::future::block_on(stop_flag.wait_stopped());

                if let Ok(mut guard) = slot_for_thread.lock() {
                    *guard = None;
                }
                drop(conn);
            })
            .ok();

        Self {
            stop,
            conn_slot,
            join,
        }
    }
}

impl Drop for NotificationServerWorker {
    fn drop(&mut self) {
        self.stop.trigger();
        if let Ok(mut guard) = self.conn_slot.lock() {
            *guard = None;
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct NotificationsIface {
    next_id: AtomicU32,
    sender: calloop::channel::Sender<NotificationEvent>,
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotificationsIface {
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next_id.fetch_add(1, Ordering::Relaxed)
        };
        let action_pairs = actions
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| (c[0].clone(), c[1].clone()))
            .collect();
        let notif = DesktopNotification {
            id,
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions: action_pairs,
            urgency: parse_urgency(&hints),
            desktop_entry: hint_string(&hints, "desktop-entry"),
            image_path: hint_string(&hints, "image-path")
                .or_else(|| hint_string(&hints, "image_path")),
            image_data: parse_image_data_hints(&hints),
            expire_timeout_ms: expire_timeout,
            received_at: SystemTime::now(),
        };
        let _ = self
            .sender
            .send(NotificationEvent::Received(Box::new(notif)));
        id
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        let _ = self.sender.send(NotificationEvent::ClosedByCaller { id });
        let _ =
            Self::notification_closed(&emitter, id, NotificationCloseReason::ClosedByCall as u32)
                .await;
    }

    async fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["body", "body-markup", "actions", "icon-static"]
    }

    async fn get_server_information(
        &self,
    ) -> (&'static str, &'static str, &'static str, &'static str) {
        (
            "waywi-notifications",
            "waywi",
            env!("CARGO_PKG_VERSION"),
            "1.2",
        )
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

fn unwrap_value<'a>(mut val: &'a Value<'a>) -> &'a Value<'a> {
    while let Value::Value(inner) = val {
        val = inner.as_ref();
    }
    val
}

fn hint_string(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let val = hints.get(key)?;
    match unwrap_value(val) {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn parse_urgency(hints: &HashMap<String, OwnedValue>) -> NotificationUrgency {
    let Some(val) = hints.get("urgency") else {
        return NotificationUrgency::Normal;
    };
    let level = match unwrap_value(val) {
        Value::U8(v) => u32::from(*v),
        Value::U32(v) => *v,
        Value::I32(v) if *v >= 0 => *v as u32,
        _ => 1,
    };
    match level {
        0 => NotificationUrgency::Low,
        2 => NotificationUrgency::Critical,
        _ => NotificationUrgency::Normal,
    }
}

fn parse_image_data_hints(hints: &HashMap<String, OwnedValue>) -> Option<NotificationImageData> {
    let raw = hints
        .get("image-data")
        .or_else(|| hints.get("image_data"))
        .or_else(|| hints.get("icon_data"))?;
    let Value::Structure(st) = unwrap_value(raw) else {
        return None;
    };
    let fields = st.fields();
    if fields.len() < 7 {
        return None;
    }
    let width = as_i32(unwrap_value(&fields[0]))?;
    let height = as_i32(unwrap_value(&fields[1]))?;
    let rowstride = as_i32(unwrap_value(&fields[2]))?;
    let Value::Bool(has_alpha) = unwrap_value(&fields[3]) else {
        return None;
    };
    let bits_per_sample = as_i32(unwrap_value(&fields[4]))?;
    let channels = as_i32(unwrap_value(&fields[5]))?;
    let Value::Array(arr) = unwrap_value(&fields[6]) else {
        return None;
    };
    let mut bytes = Vec::with_capacity(arr.len());
    for item in arr.iter() {
        if let Value::U8(b) = unwrap_value(item) {
            bytes.push(*b);
        }
    }
    Some(NotificationImageData {
        width,
        height,
        rowstride,
        has_alpha: *has_alpha,
        bits_per_sample,
        channels,
        data: bytes,
    })
}

fn as_i32(v: &Value<'_>) -> Option<i32> {
    match v {
        Value::I32(x) => Some(*x),
        Value::U32(x) => i32::try_from(*x).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_hints_parse_urgency_and_image_data() {
        let mut hints: HashMap<String, OwnedValue> = HashMap::new();
        hints.insert("urgency".to_owned(), Value::from(2u8).try_into().unwrap());
        hints.insert(
            "image-data".to_owned(),
            Value::from((
                2i32,
                1i32,
                8i32,
                true,
                8i32,
                4i32,
                vec![255u8, 0, 0, 255, 0, 255, 0, 255],
            ))
            .try_into()
            .unwrap(),
        );

        assert_eq!(parse_urgency(&hints), NotificationUrgency::Critical);
        let img = parse_image_data_hints(&hints).expect("parsed image-data");
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 1);
        assert_eq!(img.rowstride, 8);
        assert!(img.has_alpha);
        assert_eq!(img.channels, 4);
        assert_eq!(img.data.len(), 8);
    }
}
