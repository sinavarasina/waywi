//! XDG Desktop Notifications daemon (`org.freedesktop.Notifications`) support.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use bitflags::bitflags;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{DbusActionLane, DbusClient, LaneActionSpec};
use super::types::{Dbus, DbusBus};
use super::worker::DbusWorkerHandle;
use super::zvariant_helpers::unwrap_value;

const NOTIFICATIONS_BUS_NAME: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_OBJECT_PATH: &str = "/org/freedesktop/Notifications";

bitflags! {
    /// Standard XDG Desktop Notifications Specification v1.2 server capability flags advertised by
    /// `org.freedesktop.Notifications.GetCapabilities`.
    ///
    /// Applications should advertise only capabilities their UI actually implements (honest
    /// capabilities). Defaults to `BODY | BODY_MARKUP | ACTIONS | ICON_STATIC`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NotificationCapabilities: u16 {
        /// Action keys are interpreted as named icons from the active icon theme (`"action-icons"`).
        const ACTION_ICONS    = 1 << 0;
        /// Server delivers action buttons and emits `ActionInvoked` (`"actions"`).
        const ACTIONS         = 1 << 1;
        /// Server displays notification body text (`"body"`).
        const BODY            = 1 << 2;
        /// Server supports hyperlinks inside notification body markup (`"body-hyperlinks"`).
        const BODY_HYPERLINKS = 1 << 3;
        /// Server supports inline images inside notification body markup (`"body-images"`).
        const BODY_IMAGES     = 1 << 4;
        /// Server parses basic HTML markup in notification bodies (`"body-markup"`).
        const BODY_MARKUP     = 1 << 5;
        /// Server animates multi-frame icons (`"icon-multi"`).
        const ICON_MULTI      = 1 << 6;
        /// Server renders static notification icons (`"icon-static"`).
        const ICON_STATIC     = 1 << 7;
        /// Server persists notifications in a history/notification center (`"persistence"`).
        const PERSISTENCE     = 1 << 8;
        /// Server handles `sound-file` / `sound-name` hints (`"sound"`).
        const SOUND           = 1 << 9;
    }
}

impl Default for NotificationCapabilities {
    fn default() -> Self {
        Self::BODY | Self::BODY_MARKUP | Self::ACTIONS | Self::ICON_STATIC
    }
}

impl NotificationCapabilities {
    /// Converts enabled capability flags into canonical XDG Desktop Notifications v1.2 strings.
    #[must_use]
    pub fn to_xdg_strings(self) -> Vec<&'static str> {
        let mut out = Vec::with_capacity(self.bits().count_ones() as usize);
        if self.contains(Self::ACTION_ICONS) {
            out.push("action-icons");
        }
        if self.contains(Self::ACTIONS) {
            out.push("actions");
        }
        if self.contains(Self::BODY) {
            out.push("body");
        }
        if self.contains(Self::BODY_HYPERLINKS) {
            out.push("body-hyperlinks");
        }
        if self.contains(Self::BODY_IMAGES) {
            out.push("body-images");
        }
        if self.contains(Self::BODY_MARKUP) {
            out.push("body-markup");
        }
        if self.contains(Self::ICON_MULTI) {
            out.push("icon-multi");
        }
        if self.contains(Self::ICON_STATIC) {
            out.push("icon-static");
        }
        if self.contains(Self::PERSISTENCE) {
            out.push("persistence");
        }
        if self.contains(Self::SOUND) {
            out.push("sound");
        }
        out
    }
}

/// Shared lock-free capability state and live connection slot shared between [`NotificationsClient`]
/// and [`NotificationServerWorker`].
pub(crate) struct NotificationServerState {
    conn: Mutex<Option<zbus::Connection>>,
    capabilities: AtomicU16,
}

impl Default for NotificationServerState {
    fn default() -> Self {
        Self {
            conn: Mutex::new(None),
            capabilities: AtomicU16::new(NotificationCapabilities::default().bits()),
        }
    }
}

impl NotificationServerState {
    pub(crate) fn capabilities(&self) -> NotificationCapabilities {
        NotificationCapabilities::from_bits_truncate(self.capabilities.load(Ordering::Acquire))
    }

    pub(crate) fn set_capabilities(&self, caps: NotificationCapabilities) {
        self.capabilities.store(caps.bits(), Ordering::Release);
    }

    pub(crate) fn connection(&self) -> Option<zbus::Connection> {
        self.conn.lock().ok().and_then(|g| g.clone())
    }

    fn set_connection(&self, conn: Option<zbus::Connection>) {
        if let Ok(mut guard) = self.conn.lock() {
            *guard = conn;
        }
    }
}

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

/// Typed XDG desktop notification received over `org.freedesktop.Notifications.Notify`
/// (Desktop Notifications Specification v1.2).
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
    /// Urgency hint (`"urgency"`: Low / Normal / Critical).
    pub urgency: NotificationUrgency,
    /// XDG notification category hint (`"category"`, e.g. `"im.received"`, `"email.arrived"`).
    pub category: Option<String>,
    /// Sending application's `.desktop` identifier (`"desktop-entry"`).
    pub desktop_entry: Option<String>,
    /// File path or icon name hint (`"image-path"` / legacy `"image_path"`).
    pub image_path: Option<String>,
    /// Inline uncompressed pixel payload (`"image-data"` / legacy `"image_data"`, `"icon_data"`).
    pub image_data: Option<NotificationImageData>,
    /// Whether action keys should be interpreted as named icons (`"action-icons"`).
    pub action_icons: bool,
    /// Whether the notification should remain open after an action is invoked (`"resident"`).
    pub resident: bool,
    /// Whether the notification is transient and should bypass persistence/history (`"transient"`).
    pub transient: bool,
    /// Whether the sending application requests suppressing notification sounds (`"suppress-sound"`).
    pub suppress_sound: bool,
    /// Freedesktop named sound theme hint (`"sound-name"`).
    pub sound_name: Option<String>,
    /// Explicit sound file path hint (`"sound-file"`).
    pub sound_file: Option<String>,
    /// Optional target screen coordinates (`"x"`, `"y"`) when both hints are supplied.
    pub position: Option<(i32, i32)>,
    /// Requested expiration timeout in milliseconds (`-1` = server default, `0` = never expire).
    pub expire_timeout_ms: i32,
    /// Local wall-clock timestamp recorded immediately when the notification was received.
    pub received_at: SystemTime,
}

impl Default for DesktopNotification {
    fn default() -> Self {
        Self {
            id: 0,
            app_name: String::new(),
            replaces_id: 0,
            app_icon: String::new(),
            summary: String::new(),
            body: String::new(),
            actions: Vec::new(),
            urgency: NotificationUrgency::Normal,
            category: None,
            desktop_entry: None,
            image_path: None,
            image_data: None,
            action_icons: false,
            resident: false,
            transient: false,
            suppress_sound: false,
            sound_name: None,
            sound_file: None,
            position: None,
            expire_timeout_ms: -1,
            received_at: SystemTime::UNIX_EPOCH,
        }
    }
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
                capabilities: None,
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Registers an `org.freedesktop.Notifications` server advertising explicit XDG v1.2
    /// [`NotificationCapabilities`] via `GetCapabilities`.
    pub fn notifications_with_capabilities<M: 'static>(
        capabilities: NotificationCapabilities,
        map: impl Fn(NotificationEvent) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusNotifications {
                capabilities: Some(capabilities),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for XDG Desktop Notifications server controls.
#[derive(Clone, Copy)]
pub struct NotificationsClient<'a>(pub(super) &'a DbusClient);

impl NotificationsClient<'_> {
    /// Returns the currently configured [`NotificationCapabilities`] advertised by the
    /// notification server.
    #[must_use]
    pub fn capabilities(&self) -> NotificationCapabilities {
        self.0.notifications_state.capabilities()
    }

    /// Updates the XDG v1.2 [`NotificationCapabilities`] returned by
    /// `org.freedesktop.Notifications.GetCapabilities` without restarting the server.
    pub fn set_capabilities(&self, capabilities: NotificationCapabilities) {
        self.0.notifications_state.set_capabilities(capabilities);
    }

    /// Emits `org.freedesktop.Notifications.NotificationClosed(id, reason)` from the active server.
    pub fn close(&self, id: u32, reason: NotificationCloseReason) -> Result<()> {
        let conn = self.active_notifications_conn()?;
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "Notifications.NotificationClosed",
                id.to_string(),
            ),
            move |_| async move {
                let iface = conn
                    .object_server()
                    .interface::<_, NotificationsIface>(NOTIFICATIONS_OBJECT_PATH)
                    .await
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                NotificationsIface::notification_closed(iface.signal_emitter(), id, reason as u32)
                    .await
                    .map_err(|e| WaywiError::DbusError(e.to_string()))
            },
        )
    }

    /// Emits `org.freedesktop.Notifications.ActionInvoked(id, action_key)` from the active server.
    pub fn invoke_action(&self, id: u32, action_key: &str) -> Result<()> {
        let conn = self.active_notifications_conn()?;
        let key = action_key.to_owned();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "Notifications.ActionInvoked",
                id.to_string(),
            ),
            move |_| async move {
                let iface = conn
                    .object_server()
                    .interface::<_, NotificationsIface>(NOTIFICATIONS_OBJECT_PATH)
                    .await
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                NotificationsIface::action_invoked(iface.signal_emitter(), id, key.as_str())
                    .await
                    .map_err(|e| WaywiError::DbusError(e.to_string()))
            },
        )
    }

    fn active_notifications_conn(&self) -> Result<zbus::Connection> {
        self.0.notifications_state.connection().ok_or_else(|| {
            WaywiError::DbusError("org.freedesktop.Notifications server is not running".into())
        })
    }
}

pub(crate) struct NotificationServerWorker {
    state: Arc<NotificationServerState>,
    handle: DbusWorkerHandle,
}

impl NotificationServerWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<NotificationEvent>,
        state: Arc<NotificationServerState>,
    ) -> Self {
        let state_for_thread = Arc::clone(&state);
        let handle = DbusWorkerHandle::spawn("waywi-dbus-notify", move |stop_flag| {
            let iface = NotificationsIface {
                next_id: AtomicU32::new(1),
                sender: sender.clone(),
                state: Arc::clone(&state_for_thread),
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

            state_for_thread.set_connection(Some(conn.clone()));

            futures_lite::future::block_on(stop_flag.wait_stopped());

            state_for_thread.set_connection(None);
            drop(conn);
        });

        Self { state, handle }
    }
}

impl Drop for NotificationServerWorker {
    fn drop(&mut self) {
        self.handle.trigger_stop();
        self.state.set_connection(None);
    }
}

struct NotificationsIface {
    next_id: AtomicU32,
    sender: calloop::channel::Sender<NotificationEvent>,
    state: Arc<NotificationServerState>,
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
            category: hint_string(&hints, "category"),
            desktop_entry: hint_string(&hints, "desktop-entry"),
            image_path: hint_string(&hints, "image-path")
                .or_else(|| hint_string(&hints, "image_path")),
            image_data: parse_image_data_hints(&hints),
            action_icons: hint_bool(&hints, "action-icons"),
            resident: hint_bool(&hints, "resident"),
            transient: hint_bool(&hints, "transient"),
            suppress_sound: hint_bool(&hints, "suppress-sound"),
            sound_name: hint_string(&hints, "sound-name"),
            sound_file: hint_string(&hints, "sound-file"),
            position: parse_position_hints(&hints),
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
        self.state.capabilities().to_xdg_strings()
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

fn hint_string(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let val = hints.get(key)?;
    match unwrap_value(val) {
        Value::Str(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    }
}

fn hint_bool(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    let Some(val) = hints.get(key) else {
        return false;
    };
    match unwrap_value(val) {
        Value::Bool(b) => *b,
        Value::U8(v) => *v != 0,
        Value::I32(v) => *v != 0,
        Value::U32(v) => *v != 0,
        _ => false,
    }
}

fn parse_position_hints(hints: &HashMap<String, OwnedValue>) -> Option<(i32, i32)> {
    let x = as_i32(unwrap_value(hints.get("x")?))?;
    let y = as_i32(unwrap_value(hints.get("y")?))?;
    Some((x, y))
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

    #[test]
    fn notification_hints_parse_all_xdg_v1_2_standard_hints() {
        let mut hints: HashMap<String, OwnedValue> = HashMap::new();
        hints.insert(
            "category".to_owned(),
            Value::from("im.received").try_into().unwrap(),
        );
        hints.insert(
            "desktop-entry".to_owned(),
            Value::from("org.example.Chat").try_into().unwrap(),
        );
        hints.insert(
            "action-icons".to_owned(),
            Value::from(true).try_into().unwrap(),
        );
        hints.insert("resident".to_owned(), Value::from(true).try_into().unwrap());
        hints.insert("transient".to_owned(), Value::from(1u8).try_into().unwrap());
        hints.insert(
            "suppress-sound".to_owned(),
            Value::from(true).try_into().unwrap(),
        );
        hints.insert(
            "sound-name".to_owned(),
            Value::from("message-new-instant").try_into().unwrap(),
        );
        hints.insert(
            "sound-file".to_owned(),
            Value::from("/usr/share/sounds/ding.ogg")
                .try_into()
                .unwrap(),
        );
        hints.insert("x".to_owned(), Value::from(640i32).try_into().unwrap());
        hints.insert("y".to_owned(), Value::from(480i32).try_into().unwrap());

        assert_eq!(
            hint_string(&hints, "category").as_deref(),
            Some("im.received")
        );
        assert_eq!(
            hint_string(&hints, "desktop-entry").as_deref(),
            Some("org.example.Chat")
        );
        assert!(hint_bool(&hints, "action-icons"));
        assert!(hint_bool(&hints, "resident"));
        assert!(hint_bool(&hints, "transient"));
        assert!(hint_bool(&hints, "suppress-sound"));
        assert_eq!(
            hint_string(&hints, "sound-name").as_deref(),
            Some("message-new-instant")
        );
        assert_eq!(
            hint_string(&hints, "sound-file").as_deref(),
            Some("/usr/share/sounds/ding.ogg")
        );
        assert_eq!(parse_position_hints(&hints), Some((640, 480)));

        // Missing y coordinate yields None rather than a half-specified position.
        hints.remove("y");
        assert_eq!(parse_position_hints(&hints), None);

        // Empty strings normalize to None.
        hints.insert("category".to_owned(), Value::from("").try_into().unwrap());
        assert_eq!(hint_string(&hints, "category"), None);
    }

    #[test]
    fn notification_capabilities_update_lock_free() {
        assert_eq!(
            NotificationCapabilities::default().to_xdg_strings(),
            vec!["actions", "body", "body-markup", "icon-static"]
        );

        let full = NotificationCapabilities::default()
            | NotificationCapabilities::PERSISTENCE
            | NotificationCapabilities::SOUND
            | NotificationCapabilities::ACTION_ICONS;
        assert_eq!(
            full.to_xdg_strings(),
            vec![
                "action-icons",
                "actions",
                "body",
                "body-markup",
                "icon-static",
                "persistence",
                "sound",
            ]
        );

        let client = DbusClient::new();
        assert_eq!(
            client.notifications().capabilities(),
            NotificationCapabilities::default()
        );
        client.notifications().set_capabilities(full);
        assert_eq!(client.notifications().capabilities(), full);
    }
}
