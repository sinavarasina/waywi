//! Firmware (`org.freedesktop.fwupd`) and system package (`org.freedesktop.PackageKit`) update
//! inspection clients and subscriptions.

use std::collections::HashMap;

use futures_lite::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{DBUS_SLOW_TIMEOUT, DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{dict_bool, dict_str, dict_u32, get_all};

const FWUPD_DEST: &str = "org.freedesktop.fwupd";
const FWUPD_PATH: &str = "/";
const FWUPD_IFACE: &str = "org.freedesktop.fwupd";

const PK_DEST: &str = "org.freedesktop.PackageKit";
const PK_PATH: &str = "/org/freedesktop/PackageKit";
const PK_IFACE: &str = "org.freedesktop.PackageKit";
const PK_TX_IFACE: &str = "org.freedesktop.PackageKit.Transaction";

/// Device reported by `org.freedesktop.fwupd.GetDevices`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FwupdDevice {
    pub device_id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub summary: String,
}

/// Available firmware release reported by `org.freedesktop.fwupd.GetUpgrades`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FwupdUpdate {
    pub device_id: String,
    pub device_name: String,
    pub version: String,
    pub summary: String,
}

/// Snapshot of `org.freedesktop.fwupd` daemon, devices, and pending firmware updates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FwupdSnapshot {
    pub daemon_version: String,
    pub devices: Vec<FwupdDevice>,
    pub pending_updates: Vec<FwupdUpdate>,
}

/// Metadata snapshot from `org.freedesktop.PackageKit`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PackageKitSnapshot {
    pub version_major: u32,
    pub version_minor: u32,
    pub version_micro: u32,
    pub backend_name: String,
    pub distro_id: String,
    pub locked: bool,
    pub network_state: u32,
}

/// Pending package update entry returned by [`PackageKitClient::check_updates_blocking`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PackageKitUpdate {
    pub info_code: u32,
    pub package_id: String,
    pub summary: String,
}

impl PackageKitUpdate {
    /// Extracts the human package name (the first `;`-separated field of `package_id`).
    #[must_use]
    pub fn name(&self) -> &str {
        self.package_id
            .split(';')
            .next()
            .unwrap_or(&self.package_id)
    }

    /// Extracts the package version (the second `;`-separated field of `package_id`).
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.package_id.split(';').nth(1).filter(|s| !s.is_empty())
    }
}

impl Dbus {
    /// Subscribes to `org.freedesktop.fwupd` device and firmware update changes on the system bus.
    pub fn fwupd<M: 'static>(map: impl Fn(FwupdSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusFwupd {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for `org.freedesktop.fwupd`.
#[derive(Clone, Copy)]
pub struct FwupdClient<'a>(pub(super) &'a DbusClient);

impl FwupdClient<'_> {
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<FwupdSnapshot> {
        self.0.cache.fwupd.read().ok().and_then(|g| g.clone())
    }

    pub fn snapshot_blocking(&self) -> Result<FwupdSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(read_fwupd_snapshot(&conn))
    }

    /// Requests fwupd to clear/reload remote metadata (`ClearResults` / `UpdateMetadata` trigger
    /// via `SetFeatureFlags` or `Activate` is device-specific; invokes `ModifyRemote` or
    /// `GetRemotes` refresh). Triggers `org.freedesktop.fwupd.VerifyUpdate` for `device_id`.
    pub fn verify_device(&self, device_id: &str) -> Result<()> {
        if device_id.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "fwupd device_id cannot be empty".into(),
            ));
        }
        let id = device_id.to_owned();
        let target = id.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "fwupd.Verify",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(FWUPD_DEST),
                    FWUPD_PATH,
                    Some(FWUPD_IFACE),
                    "Verify",
                    &(id.as_str(),),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("fwupd.Verify failed: {e}")))?;
                Ok(())
            },
        )
    }
}

/// Zero-cost domain handle for `org.freedesktop.PackageKit`.
#[derive(Clone, Copy)]
pub struct PackageKitClient<'a>(pub(super) &'a DbusClient);

impl PackageKitClient<'_> {
    pub fn snapshot_blocking(&self) -> Result<PackageKitSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let props = get_all(&conn, PK_DEST, PK_PATH, PK_IFACE).await?;
            Ok(parse_packagekit_props(&props))
        })
    }

    /// Creates a `PackageKit` transaction, executes `GetUpdates(filter=0)`, and collects pending
    /// updates until `Finished`.
    pub fn check_updates_blocking(&self) -> Result<Vec<PackageKitUpdate>> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let tx_reply = conn
                .call_method(
                    Some(PK_DEST),
                    PK_PATH,
                    Some(PK_IFACE),
                    "CreateTransaction",
                    &(),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("PackageKit.CreateTransaction failed: {e}"))
                })?;
            let tx_path: OwnedObjectPath = tx_reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;

            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(PK_DEST)
                .and_then(|b| b.interface(PK_TX_IFACE))
                .and_then(|b| b.path(tx_path.as_str()))
                .map_err(|e| WaywiError::DbusError(e.to_string()))?
                .build();
            let mut stream = zbus::MessageStream::for_match_rule(rule, &conn, Some(128))
                .await
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;

            conn.call_method(
                Some(PK_DEST),
                tx_path.as_str(),
                Some(PK_TX_IFACE),
                "GetUpdates",
                &(0u64,),
            )
            .await
            .map_err(|e| {
                WaywiError::DbusError(format!("PackageKit.Transaction.GetUpdates failed: {e}"))
            })?;

            let mut updates = Vec::new();
            while let Some(Ok(msg)) = stream.next().await {
                match msg.header().member().map(|m| m.as_str()) {
                    Some("Package") => {
                        if let Ok((info_code, package_id, summary)) =
                            msg.body().deserialize::<(u32, String, String)>()
                        {
                            updates.push(PackageKitUpdate {
                                info_code,
                                package_id,
                                summary,
                            });
                        }
                    }
                    Some("Finished") | Some("Destroy") => break,
                    _ => {}
                }
            }
            Ok(updates)
        })
    }
}

async fn read_fwupd_snapshot(conn: &zbus::Connection) -> Result<FwupdSnapshot> {
    let props = get_all(conn, FWUPD_DEST, FWUPD_PATH, FWUPD_IFACE)
        .await
        .unwrap_or_default();
    let daemon_version = dict_str(&props, "DaemonVersion").unwrap_or_default();

    let dev_reply = conn
        .call_method(
            Some(FWUPD_DEST),
            FWUPD_PATH,
            Some(FWUPD_IFACE),
            "GetDevices",
            &(),
        )
        .await
        .map_err(|e| WaywiError::DbusError(format!("fwupd.GetDevices failed: {e}")))?;
    let raw_devs: Vec<HashMap<String, OwnedValue>> = dev_reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;

    let mut devices = Vec::with_capacity(raw_devs.len());
    let mut pending_updates = Vec::new();
    for map in &raw_devs {
        let dev = parse_fwupd_device(map);
        if !dev.device_id.is_empty()
            && let Ok(up_reply) = conn
                .call_method(
                    Some(FWUPD_DEST),
                    FWUPD_PATH,
                    Some(FWUPD_IFACE),
                    "GetUpgrades",
                    &(dev.device_id.as_str(),),
                )
                .await
            && let Ok(releases) = up_reply
                .body()
                .deserialize::<Vec<HashMap<String, OwnedValue>>>()
        {
            for rel in &releases {
                pending_updates.push(FwupdUpdate {
                    device_id: dev.device_id.clone(),
                    device_name: dev.name.clone(),
                    version: dict_str(rel, "Version").unwrap_or_default(),
                    summary: dict_str(rel, "Summary").unwrap_or_default(),
                });
            }
        }
        devices.push(dev);
    }

    Ok(FwupdSnapshot {
        daemon_version,
        devices,
        pending_updates,
    })
}

pub(crate) fn spawn_fwupd_worker(
    sender: calloop::channel::Sender<FwupdSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-fwupd",
        Some(FWUPD_DEST),
        None,
        sender,
        shared,
        |conn| async move { read_fwupd_snapshot(&conn).await.ok() },
    )
}

pub(crate) fn parse_fwupd_device(map: &HashMap<String, OwnedValue>) -> FwupdDevice {
    FwupdDevice {
        device_id: dict_str(map, "DeviceId").unwrap_or_default(),
        name: dict_str(map, "Name").unwrap_or_default(),
        vendor: dict_str(map, "Vendor").unwrap_or_default(),
        version: dict_str(map, "Version").unwrap_or_default(),
        summary: dict_str(map, "Summary").unwrap_or_default(),
    }
}

pub(crate) fn parse_packagekit_props(props: &HashMap<String, OwnedValue>) -> PackageKitSnapshot {
    PackageKitSnapshot {
        version_major: dict_u32(props, "VersionMajor").unwrap_or(0),
        version_minor: dict_u32(props, "VersionMinor").unwrap_or(0),
        version_micro: dict_u32(props, "VersionMicro").unwrap_or(0),
        backend_name: dict_str(props, "BackendName").unwrap_or_default(),
        distro_id: dict_str(props, "DistroId").unwrap_or_default(),
        locked: dict_bool(props, "Locked").unwrap_or(false),
        network_state: dict_u32(props, "NetworkState").unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn parses_fwupd_device_and_packagekit_update_id() {
        let mut dev = HashMap::new();
        dev.insert(
            "DeviceId".into(),
            OwnedValue::try_from(Value::from("abc123")).unwrap(),
        );
        dev.insert(
            "Name".into(),
            OwnedValue::try_from(Value::from("System Firmware")).unwrap(),
        );
        dev.insert(
            "Version".into(),
            OwnedValue::try_from(Value::from("1.2.3")).unwrap(),
        );
        let parsed = parse_fwupd_device(&dev);
        assert_eq!(parsed.device_id, "abc123");
        assert_eq!(parsed.name, "System Firmware");

        let upd = PackageKitUpdate {
            info_code: 4,
            package_id: "mesa;24.2.0-1;x86_64;extra".into(),
            summary: "Open-source OpenGL drivers".into(),
        };
        assert_eq!(upd.name(), "mesa");
        assert_eq!(upd.version(), Some("24.2.0-1"));
    }
}
