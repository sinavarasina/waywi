//! Optional Linux desktop services over D-Bus:
//! - CUPS printing notifications (`org.cups.cupsd.Notifier`)
//! - KDE Connect (`org.kde.kdeconnect`)
//! - Colord ICC color management (`org.freedesktop.ColorManager`)
//! - Avahi mDNS / DNS-SD (`org.freedesktop.Avahi.Server`)

use std::collections::HashMap;
use std::time::Duration;

use futures_lite::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{
    DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec, connect_shared_bus,
    decode_message_body, validate_object_path,
};
use super::types::{Dbus, DbusBus, DbusValue};
use super::worker::{DbusWorkerHandle, ServiceWatchWorker};
use super::zvariant_helpers::{dict_bool, dict_i32, dict_str, dict_str_vec, get_all};

// ---- CUPS ----

const CUPS_NOTIFIER_IFACE: &str = "org.cups.cupsd.Notifier";

/// Event emitted by CUPS (`org.cups.cupsd.Notifier` on the system bus).
#[derive(Debug, Clone, PartialEq)]
pub struct CupsEvent {
    /// Signal member name (e.g. `"JobCreated"`, `"JobCompleted"`, `"PrinterStateChanged"`).
    pub kind: String,
    pub text: String,
    pub printer_uri: String,
    pub printer_name: String,
    pub printer_state: Option<u32>,
    pub job_id: Option<u32>,
    pub job_state: Option<u32>,
    pub job_name: Option<String>,
}

// ---- KDE Connect ----

const KDEC_DEST: &str = "org.kde.kdeconnect";
const KDEC_DAEMON_PATH: &str = "/modules/kdeconnect";
const KDEC_DAEMON_IFACE: &str = "org.kde.kdeconnect.daemon";
const KDEC_DEVICE_IFACE: &str = "org.kde.kdeconnect.device";
const KDEC_BATTERY_IFACE: &str = "org.kde.kdeconnect.device.battery";
const KDEC_PING_IFACE: &str = "org.kde.kdeconnect.device.ping";
const KDEC_FIND_IFACE: &str = "org.kde.kdeconnect.device.findmyphone";
const KDEC_SHARE_IFACE: &str = "org.kde.kdeconnect.device.share";

/// Paired or reachable KDE Connect device snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KdeConnectDevice {
    pub id: String,
    pub name: String,
    pub device_type: String,
    pub is_reachable: bool,
    pub is_trusted: bool,
    pub battery_charge: Option<i32>,
    pub battery_charging: bool,
}

/// Snapshot of KDE Connect devices on the session bus.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KdeConnectSnapshot {
    pub devices: Vec<KdeConnectDevice>,
}

// ---- Colord ----

const COLORD_DEST: &str = "org.freedesktop.ColorManager";
const COLORD_PATH: &str = "/org/freedesktop/ColorManager";
const COLORD_IFACE: &str = "org.freedesktop.ColorManager";
const COLORD_DEVICE_IFACE: &str = "org.freedesktop.ColorManager.Device";
const COLORD_PROFILE_IFACE: &str = "org.freedesktop.ColorManager.Profile";

/// Color-managed display/printer/camera device from `org.freedesktop.ColorManager.Device`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColordDevice {
    pub object_path: String,
    pub device_id: String,
    pub kind: String,
    pub model: String,
    pub vendor: String,
    pub colorspace: String,
    pub profiles: Vec<String>,
}

/// ICC color profile from `org.freedesktop.ColorManager.Profile`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColordProfile {
    pub object_path: String,
    pub profile_id: String,
    pub title: String,
    pub filename: String,
    pub colorspace: String,
    pub kind: String,
}

/// Snapshot of `org.freedesktop.ColorManager` devices and profiles.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColordSnapshot {
    pub devices: Vec<ColordDevice>,
    pub profiles: Vec<ColordProfile>,
}

// ---- Avahi ----

const AVAHI_DEST: &str = "org.freedesktop.Avahi";
const AVAHI_PATH: &str = "/";
const AVAHI_SERVER_IFACE: &str = "org.freedesktop.Avahi.Server";
const AVAHI_BROWSER_IFACE: &str = "org.freedesktop.Avahi.ServiceBrowser";

/// Host/daemon summary from `org.freedesktop.Avahi.Server`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AvahiSnapshot {
    pub version: String,
    pub host_name: String,
    pub domain_name: String,
    pub host_name_fqdn: String,
    pub state: i32,
}

/// Discovered mDNS / DNS-SD service entry from `org.freedesktop.Avahi.ServiceBrowser`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AvahiServiceEntry {
    pub interface: i32,
    pub protocol: i32,
    pub name: String,
    pub service_type: String,
    pub domain: String,
}

impl Dbus {
    /// Subscribes to CUPS printing signals (`org.cups.cupsd.Notifier`) on the system bus.
    pub fn cups<M: 'static>(map: impl Fn(CupsEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusCups {
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Subscribes to KDE Connect device & battery updates (`org.kde.kdeconnect`) on the session bus.
    pub fn kdeconnect<M: 'static>(
        map: impl Fn(KdeConnectSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusKdeConnect {
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Subscribes to `org.freedesktop.ColorManager` device/profile updates on the system bus.
    pub fn colord<M: 'static>(map: impl Fn(ColordSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusColord {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for KDE Connect (`org.kde.kdeconnect`).
#[derive(Clone, Copy)]
pub struct KdeConnectClient<'a>(pub(super) &'a DbusClient);

impl KdeConnectClient<'_> {
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<KdeConnectSnapshot> {
        self.0.cache.kdeconnect.read().ok().and_then(|g| g.clone())
    }

    pub fn snapshot_blocking(&self) -> Result<KdeConnectSnapshot> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        futures_lite::future::block_on(read_kdeconnect_snapshot(&conn))
    }

    /// Sends a ping notification to `device_id` (`sendPing(message)`).
    pub fn ping(&self, device_id: &str, message: &str) -> Result<()> {
        if device_id.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "KDE Connect device_id cannot be empty".into(),
            ));
        }
        let path = format!("/modules/kdeconnect/devices/{device_id}/ping");
        let msg = message.to_owned();
        let target = device_id.to_owned();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "kdeconnect.ping.sendPing",
                target,
            ),
            move |conn| async move {
                conn.call_method(
                    Some(KDEC_DEST),
                    path.as_str(),
                    Some(KDEC_PING_IFACE),
                    "sendPing",
                    &(msg.as_str(),),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("kdeconnect sendPing failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Rings a remote device (`findmyphone.ring`).
    pub fn ring(&self, device_id: &str) -> Result<()> {
        if device_id.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "KDE Connect device_id cannot be empty".into(),
            ));
        }
        let path = format!("/modules/kdeconnect/devices/{device_id}/findmyphone");
        self.0.call_lane_method_unit(
            DbusActionLane::Desktop,
            DbusBus::Session,
            KDEC_DEST,
            &path,
            KDEC_FIND_IFACE,
            "ring",
        )
    }

    /// Shares a URL or file URI with `device_id` (`share.shareUrl`).
    pub fn share_url(&self, device_id: &str, url: &str) -> Result<()> {
        if device_id.trim().is_empty() || url.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "KDE Connect device_id and url cannot be empty".into(),
            ));
        }
        let path = format!("/modules/kdeconnect/devices/{device_id}/share");
        let url_owned = url.to_owned();
        let target = device_id.to_owned();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "kdeconnect.share.shareUrl",
                target,
            ),
            move |conn| async move {
                conn.call_method(
                    Some(KDEC_DEST),
                    path.as_str(),
                    Some(KDEC_SHARE_IFACE),
                    "shareUrl",
                    &(url_owned.as_str(),),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("kdeconnect shareUrl failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Shares plain text with `device_id` (`share.shareText`).
    pub fn share_text(&self, device_id: &str, text: &str) -> Result<()> {
        if device_id.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "KDE Connect device_id cannot be empty".into(),
            ));
        }
        let path = format!("/modules/kdeconnect/devices/{device_id}/share");
        let text_owned = text.to_owned();
        let target = device_id.to_owned();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "kdeconnect.share.shareText",
                target,
            ),
            move |conn| async move {
                conn.call_method(
                    Some(KDEC_DEST),
                    path.as_str(),
                    Some(KDEC_SHARE_IFACE),
                    "shareText",
                    &(text_owned.as_str(),),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("kdeconnect shareText failed: {e}")))?;
                Ok(())
            },
        )
    }
}

/// Zero-cost domain handle for `org.freedesktop.ColorManager`.
#[derive(Clone, Copy)]
pub struct ColordClient<'a>(pub(super) &'a DbusClient);

impl ColordClient<'_> {
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<ColordSnapshot> {
        self.0.cache.colord.read().ok().and_then(|g| g.clone())
    }

    pub fn snapshot_blocking(&self) -> Result<ColordSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(read_colord_snapshot(&conn))
    }

    /// Sets `profile_path` as the default ICC profile for `device_path` (`Device.MakeProfileDefault`).
    pub fn make_profile_default(&self, device_path: &str, profile_path: &str) -> Result<()> {
        let dev = validate_object_path(device_path, "Colord device path")?;
        let prof = validate_object_path(profile_path, "Colord profile path")?;
        let target = dev.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "ColorManager.MakeProfileDefault",
                target,
            ),
            move |conn| async move {
                let prof_obj = OwnedObjectPath::try_from(prof)
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                conn.call_method(
                    Some(COLORD_DEST),
                    dev.as_str(),
                    Some(COLORD_DEVICE_IFACE),
                    "MakeProfileDefault",
                    &(&prof_obj,),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("ColorManager.MakeProfileDefault failed: {e}"))
                })?;
                Ok(())
            },
        )
    }
}

/// Zero-cost domain handle for `org.freedesktop.Avahi.Server`.
#[derive(Clone, Copy)]
pub struct AvahiClient<'a>(pub(super) &'a DbusClient);

impl AvahiClient<'_> {
    pub fn snapshot_blocking(&self) -> Result<AvahiSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let call_str = |method: &'static str| {
                let conn = conn.clone();
                async move {
                    conn.call_method(
                        Some(AVAHI_DEST),
                        AVAHI_PATH,
                        Some(AVAHI_SERVER_IFACE),
                        method,
                        &(),
                    )
                    .await
                    .ok()
                    .and_then(|r| r.body().deserialize::<String>().ok())
                    .unwrap_or_default()
                }
            };
            let version = call_str("GetVersionString").await;
            let host_name = call_str("GetHostName").await;
            let domain_name = call_str("GetDomainName").await;
            let host_name_fqdn = call_str("GetHostNameFqdn").await;
            let state = conn
                .call_method(
                    Some(AVAHI_DEST),
                    AVAHI_PATH,
                    Some(AVAHI_SERVER_IFACE),
                    "GetState",
                    &(),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Avahi.GetState failed: {e}")))?
                .body()
                .deserialize::<i32>()
                .unwrap_or(0);
            Ok(AvahiSnapshot {
                version,
                host_name,
                domain_name,
                host_name_fqdn,
                state,
            })
        })
    }

    /// Browses mDNS services of `service_type` (e.g. `"_ipp._tcp"`, `"_ssh._tcp"`) until
    /// `AllForNow` arrives or `timeout` elapses, then frees the browser proxy cleanly.
    pub fn browse_services_blocking(
        &self,
        service_type: &str,
        domain: &str,
        timeout: Duration,
    ) -> Result<Vec<AvahiServiceEntry>> {
        if service_type.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "Avahi service_type cannot be empty".into(),
            ));
        }
        let conn = self.0.async_connection(DbusBus::System)?;
        let stype = service_type.to_owned();
        let dom = domain.to_owned();
        let (timer_id, timeout_sig) = self.0.register_timeout(timeout);
        let result = futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(AVAHI_DEST),
                    AVAHI_PATH,
                    Some(AVAHI_SERVER_IFACE),
                    "ServiceBrowserNew",
                    &(-1i32, -1i32, stype.as_str(), dom.as_str(), 0u32),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("Avahi.ServiceBrowserNew failed: {e}"))
                })?;
            let browser_path: OwnedObjectPath = reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;

            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(AVAHI_DEST)
                .and_then(|b| b.interface(AVAHI_BROWSER_IFACE))
                .and_then(|b| b.path(browser_path.as_str()))
                .map_err(|e| WaywiError::DbusError(e.to_string()))?
                .build();
            let mut stream = zbus::MessageStream::for_match_rule(rule, &conn, Some(64))
                .await
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;

            let mut entries = Vec::new();
            while !timeout_sig.is_stopped() {
                let next = futures_lite::future::or(async { Some(stream.next().await) }, async {
                    timeout_sig.wait_stopped().await;
                    None
                })
                .await;
                let Some(Some(Ok(msg))) = next else {
                    break;
                };
                match msg.header().member().map(|m| m.as_str()) {
                    Some("ItemNew") => {
                        if let Ok((interface, protocol, name, service_type, domain, _flags)) = msg
                            .body()
                            .deserialize::<(i32, i32, String, String, String, u32)>()
                        {
                            entries.push(AvahiServiceEntry {
                                interface,
                                protocol,
                                name,
                                service_type,
                                domain,
                            });
                        }
                    }
                    Some("AllForNow") | Some("Failure") => break,
                    _ => {}
                }
            }

            let _ = conn
                .call_method(
                    Some(AVAHI_DEST),
                    browser_path.as_str(),
                    Some(AVAHI_BROWSER_IFACE),
                    "Free",
                    &(),
                )
                .await;
            Ok(entries)
        });
        self.0.cancel_timeout(timer_id);
        result
    }
}

pub(crate) struct CupsWorker(#[allow(dead_code)] DbusWorkerHandle);

impl CupsWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<CupsEvent>,
        shared: DbusSharedBus,
    ) -> Self {
        Self(DbusWorkerHandle::spawn("waywi-dbus-cups", move |stop| {
            let Ok(conn) = connect_shared_bus(DbusBus::System, &shared) else {
                return;
            };
            futures_lite::future::block_on(async move {
                let rule = match zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .interface(CUPS_NOTIFIER_IFACE)
                {
                    Ok(r) => r.build(),
                    Err(_) => return,
                };
                let Ok(mut stream) =
                    zbus::MessageStream::for_match_rule(rule, &conn, Some(32)).await
                else {
                    return;
                };
                while !stop.is_stopped() {
                    let next =
                        futures_lite::future::or(async { Some(stream.next().await) }, async {
                            stop.wait_stopped().await;
                            None
                        })
                        .await;
                    let Some(Some(Ok(msg))) = next else {
                        break;
                    };
                    let kind = msg
                        .header()
                        .member()
                        .map(|m| m.to_string())
                        .unwrap_or_default();
                    let args = decode_message_body(&msg);
                    let _ = sender.send(parse_cups_signal(kind, &args));
                }
            });
        }))
    }
}

pub(crate) fn parse_cups_signal(kind: String, args: &[DbusValue]) -> CupsEvent {
    CupsEvent {
        kind,
        text: args
            .first()
            .and_then(DbusValue::as_str)
            .unwrap_or_default()
            .to_owned(),
        printer_uri: args
            .get(1)
            .and_then(DbusValue::as_str)
            .unwrap_or_default()
            .to_owned(),
        printer_name: args
            .get(2)
            .and_then(DbusValue::as_str)
            .unwrap_or_default()
            .to_owned(),
        printer_state: args.get(3).and_then(DbusValue::as_u32),
        job_id: args.get(6).and_then(DbusValue::as_u32),
        job_state: args.get(7).and_then(DbusValue::as_u32),
        job_name: args
            .get(9)
            .and_then(DbusValue::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    }
}

async fn read_kdeconnect_snapshot(conn: &zbus::Connection) -> Result<KdeConnectSnapshot> {
    let reply = conn
        .call_method(
            Some(KDEC_DEST),
            KDEC_DAEMON_PATH,
            Some(KDEC_DAEMON_IFACE),
            "devices",
            &(false, false),
        )
        .await
        .map_err(|e| WaywiError::DbusError(format!("kdeconnect.devices failed: {e}")))?;
    let ids: Vec<String> = reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;

    let mut devices = Vec::with_capacity(ids.len());
    for id in ids.into_iter().take(32) {
        let dev_path = format!("/modules/kdeconnect/devices/{id}");
        let props = get_all(conn, KDEC_DEST, &dev_path, KDEC_DEVICE_IFACE)
            .await
            .unwrap_or_default();
        let bat_path = format!("{dev_path}/battery");
        let bat_props = get_all(conn, KDEC_DEST, &bat_path, KDEC_BATTERY_IFACE)
            .await
            .ok();
        let battery_charge = bat_props
            .as_ref()
            .and_then(|p| dict_i32(p, "charge"))
            .filter(|c| *c >= 0);
        let battery_charging = bat_props
            .as_ref()
            .and_then(|p| dict_bool(p, "isCharging"))
            .unwrap_or(false);
        devices.push(KdeConnectDevice {
            id,
            name: dict_str(&props, "name").unwrap_or_default(),
            device_type: dict_str(&props, "type").unwrap_or_default(),
            is_reachable: dict_bool(&props, "isReachable").unwrap_or(false),
            is_trusted: dict_bool(&props, "isTrusted")
                .or_else(|| dict_bool(&props, "isPaired"))
                .unwrap_or(false),
            battery_charge,
            battery_charging,
        });
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    Ok(KdeConnectSnapshot { devices })
}

pub(crate) fn spawn_kdeconnect_worker(
    sender: calloop::channel::Sender<KdeConnectSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::Session,
        "waywi-dbus-kdeconnect",
        Some(KDEC_DEST),
        None,
        sender,
        shared,
        |conn| async move { read_kdeconnect_snapshot(&conn).await.ok() },
    )
}

async fn read_colord_snapshot(conn: &zbus::Connection) -> Result<ColordSnapshot> {
    let dev_reply = conn
        .call_method(
            Some(COLORD_DEST),
            COLORD_PATH,
            Some(COLORD_IFACE),
            "GetDevices",
            &(),
        )
        .await
        .map_err(|e| WaywiError::DbusError(format!("ColorManager.GetDevices failed: {e}")))?;
    let dev_paths: Vec<OwnedObjectPath> = dev_reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;

    let mut devices = Vec::new();
    for path in dev_paths.into_iter().take(64) {
        if let Ok(props) = get_all(conn, COLORD_DEST, path.as_str(), COLORD_DEVICE_IFACE).await {
            devices.push(parse_colord_device(path.to_string(), &props));
        }
    }

    let prof_reply = conn
        .call_method(
            Some(COLORD_DEST),
            COLORD_PATH,
            Some(COLORD_IFACE),
            "GetProfiles",
            &(),
        )
        .await
        .ok();
    let mut profiles = Vec::new();
    if let Some(reply) = prof_reply
        && let Ok(prof_paths) = reply.body().deserialize::<Vec<OwnedObjectPath>>()
    {
        for path in prof_paths.into_iter().take(128) {
            if let Ok(props) = get_all(conn, COLORD_DEST, path.as_str(), COLORD_PROFILE_IFACE).await
            {
                profiles.push(parse_colord_profile(path.to_string(), &props));
            }
        }
    }

    Ok(ColordSnapshot { devices, profiles })
}

pub(crate) fn spawn_colord_worker(
    sender: calloop::channel::Sender<ColordSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-colord",
        Some(COLORD_DEST),
        None,
        sender,
        shared,
        |conn| async move { read_colord_snapshot(&conn).await.ok() },
    )
}

pub(crate) fn parse_colord_device(
    object_path: String,
    props: &HashMap<String, OwnedValue>,
) -> ColordDevice {
    ColordDevice {
        object_path,
        device_id: dict_str(props, "DeviceId").unwrap_or_default(),
        kind: dict_str(props, "Kind").unwrap_or_default(),
        model: dict_str(props, "Model").unwrap_or_default(),
        vendor: dict_str(props, "Vendor").unwrap_or_default(),
        colorspace: dict_str(props, "Colorspace").unwrap_or_default(),
        profiles: dict_str_vec(props, "Profiles"),
    }
}

pub(crate) fn parse_colord_profile(
    object_path: String,
    props: &HashMap<String, OwnedValue>,
) -> ColordProfile {
    ColordProfile {
        object_path,
        profile_id: dict_str(props, "ProfileId").unwrap_or_default(),
        title: dict_str(props, "Title").unwrap_or_default(),
        filename: dict_str(props, "Filename").unwrap_or_default(),
        colorspace: dict_str(props, "Colorspace").unwrap_or_default(),
        kind: dict_str(props, "Kind").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn parses_cups_signal_and_colord_properties() {
        let ev = parse_cups_signal(
            "JobCreated".into(),
            &[
                DbusValue::String("Job #7 created".into()),
                DbusValue::String("ipp://localhost/printers/HP".into()),
                DbusValue::String("HP".into()),
                DbusValue::U32(3),
                DbusValue::String("none".into()),
                DbusValue::Bool(true),
                DbusValue::U32(7),
                DbusValue::U32(5),
                DbusValue::String("processing".into()),
                DbusValue::String("invoice.pdf".into()),
            ],
        );
        assert_eq!(ev.kind, "JobCreated");
        assert_eq!(ev.printer_name, "HP");
        assert_eq!(ev.job_id, Some(7));
        assert_eq!(ev.job_name.as_deref(), Some("invoice.pdf"));

        let mut dev = HashMap::new();
        dev.insert(
            "DeviceId".into(),
            OwnedValue::try_from(Value::from("xrandr-eDP-1")).unwrap(),
        );
        dev.insert(
            "Kind".into(),
            OwnedValue::try_from(Value::from("display")).unwrap(),
        );
        let parsed = parse_colord_device("/org/freedesktop/ColorManager/devices/1".into(), &dev);
        assert_eq!(parsed.device_id, "xrandr-eDP-1");
        assert_eq!(parsed.kind, "display");
    }
}
