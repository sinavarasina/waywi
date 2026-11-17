//! System tray (`org.kde.StatusNotifierWatcher`, `org.kde.StatusNotifierItem`, and
//! `com.canonical.dbusmenu`) host and watcher support.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{
    DbusActionLane, DbusClient, LaneActionSpec, validate_bus_name, validate_object_path,
};
use super::types::{Dbus, DbusBus};
use super::worker::AsyncStopSignal;

const SNW_BUS: &str = "org.kde.StatusNotifierWatcher";
const SNW_PATH: &str = "/StatusNotifierWatcher";
const SNW_IFACE: &str = "org.kde.StatusNotifierWatcher";
const SNI_IFACE: &str = "org.kde.StatusNotifierItem";
const DBUSMENU_IFACE: &str = "com.canonical.dbusmenu";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

/// Functional category advertised by a `StatusNotifierItem`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TrayCategory {
    #[default]
    ApplicationStatus,
    Communications,
    SystemServices,
    Hardware,
}

impl TrayCategory {
    /// Parses an SNI `Category` property string.
    #[must_use]
    pub fn from_sni_str(s: &str) -> Self {
        match s {
            "Communications" => Self::Communications,
            "SystemServices" => Self::SystemServices,
            "Hardware" => Self::Hardware,
            _ => Self::ApplicationStatus,
        }
    }
}

/// Visibility/urgency status advertised by a `StatusNotifierItem`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TrayStatus {
    Passive,
    #[default]
    Active,
    NeedsAttention,
}

impl TrayStatus {
    /// Parses an SNI `Status` property string.
    #[must_use]
    pub fn from_sni_str(s: &str) -> Self {
        match s {
            "Passive" => Self::Passive,
            "NeedsAttention" => Self::NeedsAttention,
            _ => Self::Active,
        }
    }
}

/// In-memory pixmap delivered by a tray item (`a(iiay)` in network-byte-order ARGB32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayIconPixmap {
    pub width: i32,
    pub height: i32,
    pub argb32: Vec<u8>,
}

impl TrayIconPixmap {
    /// Converts SNI network-order `ARGB32` (`[A, R, G, B]`) pixels into straight `RGBA8888` bytes
    /// suitable for [`EffectCtx::load_rgba_image`](crate::EffectCtx::load_rgba_image).
    #[must_use]
    pub fn to_rgba(&self) -> Option<Vec<u8>> {
        let w = usize::try_from(self.width).ok()?;
        let h = usize::try_from(self.height).ok()?;
        let expected = w.checked_mul(h)?.checked_mul(4)?;
        if w == 0 || h == 0 || self.argb32.len() < expected {
            return None;
        }
        let mut out = vec![0u8; expected];
        for (src, dst) in self.argb32[..expected]
            .as_chunks::<4>()
            .0
            .iter()
            .zip(out.as_chunks_mut::<4>().0)
        {
            dst[0] = src[1];
            dst[1] = src[2];
            dst[2] = src[3];
            dst[3] = src[0];
        }
        Some(out)
    }

    /// Selects the closest-sized pixmap >= `target_px` (or largest available).
    #[must_use]
    pub fn best_for_size(pixmaps: &[Self], target_px: i32) -> Option<&Self> {
        pixmaps
            .iter()
            .filter(|p| p.width > 0 && p.height > 0)
            .min_by_key(|p| {
                let side = p.width.max(p.height);
                if side >= target_px {
                    (0, side - target_px)
                } else {
                    (1, target_px - side)
                }
            })
    }
}

/// Structured tooltip metadata reported by a `StatusNotifierItem`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrayToolTip {
    pub icon_name: String,
    pub title: String,
    pub description: String,
}

/// Menu node retrieved from a `com.canonical.dbusmenu` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenuItem {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub is_separator: bool,
    pub toggle_type: Option<String>,
    pub toggle_state: i32,
    pub children: Vec<TrayMenuItem>,
}

/// Full typed snapshot of a registered system tray item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayItemSnapshot {
    /// Canonical `"<bus_name><object_path>"` identifier tracked by StatusNotifierWatcher.
    pub service: String,
    pub bus_name: String,
    pub object_path: String,
    pub id: String,
    pub title: String,
    pub category: TrayCategory,
    pub status: TrayStatus,
    pub icon_name: String,
    pub icon_theme_path: Option<String>,
    pub icon_pixmap: Vec<TrayIconPixmap>,
    pub attention_icon_name: String,
    pub overlay_icon_name: String,
    pub tooltip: Option<TrayToolTip>,
    pub menu_path: Option<String>,
    pub item_is_menu: bool,
}

/// Events emitted by [`Dbus::tray`] when watching system tray items.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEvent {
    /// Emitted once the local watcher or StatusNotifierHost connection is ready.
    WatcherReady { is_watcher_owner: bool },
    /// A new tray item registered and was introspected cleanly.
    ItemRegistered(TrayItemSnapshot),
    /// An existing tray item updated its icon, status, title, or tooltip.
    ItemUpdated(TrayItemSnapshot),
    /// A tray item unregistered or its bus owner disconnected.
    ItemUnregistered { service: String },
    /// Recoverable session/watcher error.
    Error(String),
}

impl Dbus {
    /// Registers a `StatusNotifierWatcher` (or attaches as a `StatusNotifierHost` if one is
    /// already active) on the session bus and streams typed tray item updates.
    pub fn tray<M: 'static>(map: impl Fn(TrayEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusTray {
                key: "dbus:status-notifier-tray".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Lists registered SNI item identifiers from the active `org.kde.StatusNotifierWatcher`.
    pub fn tray_list_items(&self) -> Result<Vec<String>> {
        let conn = self.connection(DbusBus::Session)?;
        let reply = conn
            .call_method(
                Some(SNW_BUS),
                SNW_PATH,
                Some(PROPS_IFACE),
                "Get",
                &(SNW_IFACE, "RegisteredStatusNotifierItems"),
            )
            .map_err(|e| {
                WaywiError::DbusError(format!("StatusNotifierWatcher query failed: {e}"))
            })?;
        let val: OwnedValue = reply
            .body()
            .deserialize()
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        Vec::<String>::try_from(val).map_err(|e| WaywiError::DbusError(e.to_string()))
    }

    /// Reads the current [`TrayItemSnapshot`] for `service` (either `"bus/path"` or `"bus"`).
    pub fn tray_snapshot(&self, service: &str) -> Result<TrayItemSnapshot> {
        let (bus_name, object_path) = split_sni_service(service, None);
        let conn = self.connection(DbusBus::Session)?;
        let reply = conn
            .call_method(
                Some(bus_name.as_str()),
                object_path.as_str(),
                Some(PROPS_IFACE),
                "GetAll",
                &(SNI_IFACE,),
            )
            .map_err(|e| {
                WaywiError::DbusError(format!(
                    "StatusNotifierItem GetAll failed for {service}: {e}"
                ))
            })?;
        let props: HashMap<String, OwnedValue> = reply
            .body()
            .deserialize()
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        Ok(parse_tray_item_props(bus_name, object_path, &props))
    }

    fn call_sni_action<B>(&self, service: &str, method: &'static str, body: B) -> Result<()>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType + Send + Sync + 'static,
    {
        let _ = validate_bus_name(service, "tray service")?;
        let (dest, path) = split_sni_service(service, None);
        let op = format!("StatusNotifierItem.{method}");
        let target = service.to_owned();
        self.submit_lane_action(
            LaneActionSpec::fast(DbusActionLane::Desktop, DbusBus::Session, op, target),
            move |conn| async move {
                conn.call_method(
                    Some(dest.as_str()),
                    path.as_str(),
                    Some(SNI_IFACE),
                    method,
                    &body,
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("tray {method} failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Sends left-click `Activate(x, y)` to a tray item.
    pub fn tray_activate(&self, service: &str, x: i32, y: i32) -> Result<()> {
        self.call_sni_action(service, "Activate", (x, y))
    }

    /// Sends middle-click `SecondaryActivate(x, y)` to a tray item.
    pub fn tray_secondary_activate(&self, service: &str, x: i32, y: i32) -> Result<()> {
        self.call_sni_action(service, "SecondaryActivate", (x, y))
    }

    /// Sends right-click `ContextMenu(x, y)` to a tray item.
    pub fn tray_context_menu(&self, service: &str, x: i32, y: i32) -> Result<()> {
        self.call_sni_action(service, "ContextMenu", (x, y))
    }

    /// Sends `Scroll(delta, orientation)` (`"vertical"` or `"horizontal"`) to a tray item.
    pub fn tray_scroll(&self, service: &str, delta: i32, orientation: &str) -> Result<()> {
        self.call_sni_action(service, "Scroll", (delta, orientation.to_owned()))
    }

    /// Reads top-level `com.canonical.dbusmenu` entries from a tray item's `menu_path`.
    pub fn tray_menu_layout(&self, bus_name: &str, menu_path: &str) -> Result<Vec<TrayMenuItem>> {
        let conn = self.connection(DbusBus::Session)?;
        let empty_props: Vec<&str> = Vec::new();
        let reply = conn
            .call_method(
                Some(bus_name),
                menu_path,
                Some(DBUSMENU_IFACE),
                "GetLayout",
                &(0i32, -1i32, empty_props),
            )
            .map_err(|e| WaywiError::DbusError(format!("dbusmenu GetLayout failed: {e}")))?;
        type DbusMenuLayoutNode = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
        let (_rev, root): (u32, DbusMenuLayoutNode) = reply
            .body()
            .deserialize()
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        Ok(root.2.iter().filter_map(parse_dbusmenu_node).collect())
    }

    /// Sends a `com.canonical.dbusmenu.Event` (e.g. `"clicked"` / `"hovered"`) for `item_id`.
    pub fn tray_menu_event(
        &self,
        bus_name: &str,
        menu_path: &str,
        item_id: i32,
        event_id: &str,
    ) -> Result<()> {
        let bus = validate_bus_name(bus_name, "dbusmenu bus_name")?;
        let path = validate_object_path(menu_path, "dbusmenu menu_path")?;
        let target = path.clone();
        let ev = validate_bus_name(event_id, "dbusmenu event_id")?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0u32, |d| d.as_secs() as u32);
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "dbusmenu.Event",
                target,
            ),
            move |conn| async move {
                conn.call_method(
                    Some(bus.as_str()),
                    path.as_str(),
                    Some(DBUSMENU_IFACE),
                    "Event",
                    &(item_id, ev.as_str(), Value::from(0i32), ts),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("dbusmenu Event failed: {e}")))?;
                Ok(())
            },
        )
    }
}

/// Splits an SNI registration string (`":1.20/StatusNotifierItem"`, `":1.20"`, or `"/org/..."`)
/// cleanly into `(bus_name, object_path)`.
pub(crate) fn split_sni_service(service: &str, header_sender: Option<&str>) -> (String, String) {
    if let Some(slash) = service.find('/') {
        if slash == 0 {
            let bus = header_sender.unwrap_or(service).to_owned();
            (bus, service.to_owned())
        } else {
            (service[..slash].to_owned(), service[slash..].to_owned())
        }
    } else {
        (service.to_owned(), "/StatusNotifierItem".to_owned())
    }
}

fn canonical_sni_id(bus_name: &str, object_path: &str) -> String {
    format!("{bus_name}{object_path}")
}

fn parse_tray_item_props(
    bus_name: String,
    object_path: String,
    props: &HashMap<String, OwnedValue>,
) -> TrayItemSnapshot {
    let service = canonical_sni_id(&bus_name, &object_path);
    let id = str_prop(props, "Id").unwrap_or_default();
    let title = str_prop(props, "Title").unwrap_or_default();
    let category = str_prop(props, "Category")
        .map(|s| TrayCategory::from_sni_str(&s))
        .unwrap_or_default();
    let status = str_prop(props, "Status")
        .map(|s| TrayStatus::from_sni_str(&s))
        .unwrap_or_default();
    let icon_name = str_prop(props, "IconName").unwrap_or_default();
    let icon_theme_path = str_prop(props, "IconThemePath").filter(|s| !s.is_empty());
    let attention_icon_name = str_prop(props, "AttentionIconName").unwrap_or_default();
    let overlay_icon_name = str_prop(props, "OverlayIconName").unwrap_or_default();
    let item_is_menu = props
        .get("ItemIsMenu")
        .and_then(|v| bool::try_from(&**v).ok())
        .unwrap_or(false);
    let menu_path = props
        .get("Menu")
        .and_then(|v| OwnedObjectPath::try_from(v.clone()).ok())
        .map(|p| p.to_string())
        .filter(|s| s != "/" && !s.is_empty());
    let icon_pixmap = parse_pixmap_array(props.get("IconPixmap"));
    let tooltip = parse_tooltip(props.get("ToolTip"));

    TrayItemSnapshot {
        service,
        bus_name,
        object_path,
        id,
        title,
        category,
        status,
        icon_name,
        icon_theme_path,
        icon_pixmap,
        attention_icon_name,
        overlay_icon_name,
        tooltip,
        menu_path,
        item_is_menu,
    }
}

fn str_prop(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(&**v).ok())
        .map(str::to_owned)
}

fn parse_pixmap_array(val: Option<&OwnedValue>) -> Vec<TrayIconPixmap> {
    let Some(v) = val else {
        return Vec::new();
    };
    let Ok(items) = Vec::<(i32, i32, Vec<u8>)>::try_from(v.clone()) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter(|(w, h, bytes)| *w > 0 && *h > 0 && !bytes.is_empty())
        .map(|(width, height, argb32)| TrayIconPixmap {
            width,
            height,
            argb32,
        })
        .collect()
}

fn parse_tooltip(val: Option<&OwnedValue>) -> Option<TrayToolTip> {
    let v = val?;
    let (icon_name, _pixmaps, title, description) =
        <(String, Vec<(i32, i32, Vec<u8>)>, String, String)>::try_from(v.clone()).ok()?;
    if icon_name.is_empty() && title.is_empty() && description.is_empty() {
        return None;
    }
    Some(TrayToolTip {
        icon_name,
        title,
        description,
    })
}

fn parse_dbusmenu_node(val: &OwnedValue) -> Option<TrayMenuItem> {
    let Value::Structure(st) = &**val else {
        return None;
    };
    parse_dbusmenu_struct(st)
}

fn parse_dbusmenu_struct(st: &zbus::zvariant::Structure<'_>) -> Option<TrayMenuItem> {
    let fields = st.fields();
    if fields.len() < 3 {
        return None;
    }
    let Value::I32(id) = fields[0] else {
        return None;
    };
    let Value::Dict(ref dict) = fields[1] else {
        return None;
    };
    let mut label = String::new();
    let mut enabled = true;
    let mut visible = true;
    let mut is_separator = false;
    let mut toggle_type = None;
    let mut toggle_state = -1i32;

    for (k, v) in dict.iter() {
        let Value::Str(ks) = k else { continue };
        let inner = super::zvariant_helpers::unwrap_value(v);
        match ks.as_str() {
            "label" => {
                if let Value::Str(s) = inner {
                    label = s.replace('_', "");
                }
            }
            "enabled" => {
                if let Value::Bool(b) = inner {
                    enabled = *b;
                }
            }
            "visible" => {
                if let Value::Bool(b) = inner {
                    visible = *b;
                }
            }
            "type" => {
                if let Value::Str(s) = inner {
                    is_separator = s.as_str() == "separator";
                }
            }
            "toggle-type" => {
                if let Value::Str(s) = inner
                    && !s.is_empty()
                {
                    toggle_type = Some(s.to_string());
                }
            }
            "toggle-state" => {
                if let Value::I32(n) = inner {
                    toggle_state = *n;
                }
            }
            _ => {}
        }
    }

    let children = if let Value::Array(ref arr) = fields[2] {
        arr.iter()
            .filter_map(|child| match super::zvariant_helpers::unwrap_value(child) {
                Value::Structure(cst) => parse_dbusmenu_struct(cst),
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    };

    Some(TrayMenuItem {
        id,
        label,
        enabled,
        visible,
        is_separator,
        toggle_type,
        toggle_state,
        children,
    })
}

#[derive(Default)]
struct WatcherRegistry {
    items: Vec<String>,
    hosts: Vec<String>,
}

struct StatusNotifierWatcherIface {
    registry: Arc<Mutex<WatcherRegistry>>,
    internal_tx: std::sync::mpsc::Sender<String>,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl StatusNotifierWatcherIface {
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        let header_sender = header.sender().map(|s| s.to_string());
        let (bus, path) = split_sni_service(&service, header_sender.as_deref());
        let id = canonical_sni_id(&bus, &path);
        let inserted = {
            let mut reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            if reg.items.iter().any(|x| x == &id) {
                false
            } else {
                reg.items.push(id.clone());
                true
            }
        };
        if inserted {
            let _ = Self::status_notifier_item_registered(&emitter, &id).await;
            let _ = self.internal_tx.send(id);
        }
    }

    async fn register_status_notifier_host(
        &self,
        service: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        let inserted = {
            let mut reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            if reg.hosts.iter().any(|x| x == &service) {
                false
            } else {
                reg.hosts.push(service);
                true
            }
        };
        if inserted {
            let _ = Self::status_notifier_host_registered(&emitter).await;
        }
    }

    #[zbus(property)]
    async fn registered_status_notifier_items(&self) -> Vec<String> {
        let reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        reg.items.clone()
    }

    #[zbus(property)]
    async fn is_status_notifier_host_registered(&self) -> bool {
        let reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        !reg.hosts.is_empty()
    }

    #[zbus(property)]
    async fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

pub(crate) struct TrayWorker {
    stop: AsyncStopSignal,
    join: Option<std::thread::JoinHandle<()>>,
}

impl TrayWorker {
    pub(crate) fn spawn(sender: calloop::channel::Sender<TrayEvent>) -> Self {
        let stop = AsyncStopSignal::new();
        let stop_sig = stop.clone();
        let join = std::thread::Builder::new()
            .name("waywi-dbus-tray".to_owned())
            .spawn(move || {
                futures_lite::future::block_on(run_tray_worker(sender, stop_sig));
            })
            .ok();
        Self { stop, join }
    }
}

impl Drop for TrayWorker {
    fn drop(&mut self) {
        self.stop.trigger();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

async fn run_tray_worker(sender: calloop::channel::Sender<TrayEvent>, stop_sig: AsyncStopSignal) {
    let registry = Arc::new(Mutex::new(WatcherRegistry {
        items: Vec::new(),
        hosts: Vec::new(),
    }));
    let (local_reg_tx, local_reg_rx) = std::sync::mpsc::channel::<String>();
    let iface = StatusNotifierWatcherIface {
        registry: Arc::clone(&registry),
        internal_tx: local_reg_tx,
    };

    let watcher_attempt = match zbus::connection::Builder::session() {
        Ok(b) => match b.name(SNW_BUS) {
            Ok(b2) => match b2.serve_at(SNW_PATH, iface) {
                Ok(b3) => b3.build().await,
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };

    let (conn, is_watcher_owner) = match watcher_attempt {
        Ok(c) => (c, true),
        Err(_) => match zbus::Connection::session().await {
            Ok(c) => (c, false),
            Err(e) => {
                let _ = sender.send(TrayEvent::Error(format!(
                    "failed to connect session D-Bus for tray: {e}"
                )));
                return;
            }
        },
    };

    let host_name = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    if !is_watcher_owner {
        let _ = conn.request_name(host_name.as_str()).await;
        let _ = conn
            .call_method(
                Some(SNW_BUS),
                SNW_PATH,
                Some(SNW_IFACE),
                "RegisterStatusNotifierHost",
                &(host_name.as_str(),),
            )
            .await;
    } else if let Ok(mut reg) = registry.lock() {
        reg.hosts.push(host_name);
    }

    let _ = sender.send(TrayEvent::WatcherReady { is_watcher_owner });

    let mut tracked: HashMap<String, (Option<String>, String, String)> = HashMap::new();
    if !is_watcher_owner
        && let Ok(reply) = conn
            .call_method(
                Some(SNW_BUS),
                SNW_PATH,
                Some(PROPS_IFACE),
                "Get",
                &(SNW_IFACE, "RegisteredStatusNotifierItems"),
            )
            .await
        && let Ok(val) = reply.body().deserialize::<OwnedValue>()
        && let Ok(existing) = Vec::<String>::try_from(val)
    {
        for service in existing {
            track_and_emit_sni(&conn, &mut tracked, &sender, &service).await;
        }
    }

    // Listen to NameOwnerChanged, StatusNotifierWatcher signals, and StatusNotifierItem update signals.
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .build();
    let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule, &conn, Some(128)).await else {
        return;
    };

    use futures_lite::StreamExt;
    while !stop_sig.is_stopped() {
        while let Ok(new_service) = local_reg_rx.try_recv() {
            track_and_emit_sni(&conn, &mut tracked, &sender, &new_service).await;
        }

        let next_msg = futures_lite::future::or(async { Some(stream.next().await) }, async {
            stop_sig.wait_stopped().await;
            None
        })
        .await;

        while let Ok(new_service) = local_reg_rx.try_recv() {
            track_and_emit_sni(&conn, &mut tracked, &sender, &new_service).await;
        }

        let Some(Some(Ok(msg))) = next_msg else {
            break;
        };

        let header = msg.header();
        let iface = header.interface().map(|i| i.as_str()).unwrap_or("");
        let member = header.member().map(|m| m.as_str()).unwrap_or("");

        match (iface, member) {
            (SNW_IFACE, "StatusNotifierItemRegistered") if !is_watcher_owner => {
                if let Ok(svc) = msg.body().deserialize::<String>() {
                    track_and_emit_sni(&conn, &mut tracked, &sender, &svc).await;
                }
            }
            (SNW_IFACE, "StatusNotifierItemUnregistered") if !is_watcher_owner => {
                if let Ok(svc) = msg.body().deserialize::<String>() {
                    let (bus, path) = split_sni_service(&svc, None);
                    let canon = canonical_sni_id(&bus, &path);
                    let removed = if tracked.remove(&canon).is_some() {
                        Some(canon)
                    } else if tracked.remove(&svc).is_some() {
                        Some(svc)
                    } else {
                        None
                    };
                    if let Some(service) = removed {
                        let _ = sender.send(TrayEvent::ItemUnregistered { service });
                    }
                }
            }
            (
                SNI_IFACE,
                "NewIcon" | "NewAttentionIcon" | "NewOverlayIcon" | "NewStatus" | "NewTitle"
                | "NewToolTip",
            ) => {
                let msg_sender = header.sender().map(|s| s.as_str()).unwrap_or("");
                let msg_path = header.path().map(|p| p.as_str()).unwrap_or("");
                let mut matched = Vec::new();
                for (canon, (owner, bus, path)) in &tracked {
                    let sender_match = msg_sender.is_empty()
                        || bus == msg_sender
                        || owner.as_deref() == Some(msg_sender);
                    if sender_match && path == msg_path {
                        matched.push((canon.clone(), bus.clone(), path.clone()));
                    }
                }
                for (_canon, bus, path) in matched {
                    if let Some(snap) = fetch_tray_snapshot_async(&conn, &bus, &path).await {
                        let _ = sender.send(TrayEvent::ItemUpdated(snap));
                    }
                }
            }
            ("org.freedesktop.DBus", "NameOwnerChanged") => {
                if let Ok((name, _old_owner, new_owner)) =
                    msg.body().deserialize::<(String, String, String)>()
                    && new_owner.is_empty()
                {
                    let vanished: Vec<String> = tracked
                        .iter()
                        .filter(|(_, (owner, bus, _))| {
                            bus == &name || owner.as_deref() == Some(name.as_str())
                        })
                        .map(|(k, _)| k.clone())
                        .collect();
                    for canon in vanished {
                        tracked.remove(&canon);
                        if is_watcher_owner {
                            if let Ok(mut reg) = registry.lock() {
                                reg.items.retain(|x| x != &canon);
                            }
                            if let Ok(iface_ref) = conn
                                .object_server()
                                .interface::<_, StatusNotifierWatcherIface>(SNW_PATH)
                                .await
                            {
                                let _ =
                                    StatusNotifierWatcherIface::status_notifier_item_unregistered(
                                        iface_ref.signal_emitter(),
                                        &canon,
                                    )
                                    .await;
                            }
                        }
                        let _ = sender.send(TrayEvent::ItemUnregistered { service: canon });
                    }
                }
            }
            _ => {}
        }
    }
}

async fn resolve_sni_owner(conn: &zbus::Connection, bus: &str) -> Option<String> {
    if bus.starts_with(':') {
        return Some(bus.to_owned());
    }
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(bus,),
        )
        .await
        .ok()?;
    reply.body().deserialize::<String>().ok()
}

async fn track_and_emit_sni(
    conn: &zbus::Connection,
    tracked: &mut HashMap<String, (Option<String>, String, String)>,
    sender: &calloop::channel::Sender<TrayEvent>,
    raw_service: &str,
) {
    let (bus, path) = split_sni_service(raw_service, None);
    let canon = canonical_sni_id(&bus, &path);
    if tracked.contains_key(&canon) {
        return;
    }
    let owner = resolve_sni_owner(conn, &bus).await;
    if let Some(ref ow) = owner
        && tracked.values().any(|(existing_owner, _, existing_path)| {
            existing_owner.as_deref() == Some(ow.as_str()) && existing_path == &path
        })
    {
        return;
    }
    if let Some(snap) = fetch_tray_snapshot_async(conn, &bus, &path).await {
        tracked.insert(canon, (owner, bus, path));
        let _ = sender.send(TrayEvent::ItemRegistered(snap));
    }
}

async fn fetch_tray_snapshot_async(
    conn: &zbus::Connection,
    bus: &str,
    path: &str,
) -> Option<TrayItemSnapshot> {
    let reply = conn
        .call_method(Some(bus), path, Some(PROPS_IFACE), "GetAll", &(SNI_IFACE,))
        .await
        .ok()?;
    let props: HashMap<String, OwnedValue> = reply.body().deserialize().ok()?;
    Some(parse_tray_item_props(
        bus.to_owned(),
        path.to_owned(),
        &props,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sni_service_and_converts_argb_pixmap() {
        assert_eq!(
            split_sni_service(":1.45/org/ayatana/NotificationItem/nm", None),
            (
                ":1.45".to_owned(),
                "/org/ayatana/NotificationItem/nm".to_owned()
            )
        );
        assert_eq!(
            split_sni_service("/org/ayatana/NotificationItem/app", Some(":1.99")),
            (
                ":1.99".to_owned(),
                "/org/ayatana/NotificationItem/app".to_owned()
            )
        );
        assert_eq!(
            split_sni_service(":1.77", None),
            (":1.77".to_owned(), "/StatusNotifierItem".to_owned())
        );

        let pix = TrayIconPixmap {
            width: 1,
            height: 1,
            argb32: vec![0x80, 0x11, 0x22, 0x33],
        };
        assert_eq!(pix.to_rgba(), Some(vec![0x11, 0x22, 0x33, 0x80]));
    }
}
