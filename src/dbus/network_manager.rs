//! System NetworkManager inspection and control (`org.freedesktop.NetworkManager`).

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{DbusClient, DbusSharedBus, connect_shared_bus};
use super::types::{Dbus, DbusBus};
use super::worker::AsyncStopSignal;
use super::zvariant_helpers::{
    dict_bool, dict_bytes, dict_object_path, dict_str, dict_str_vec, dict_u8, dict_u32,
    get_all_async, get_all_blocking,
};

const NM_DEST: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const NM_DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const NM_WIRELESS_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const NM_AP_IFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
const NM_ACTIVE_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const NM_IP4_IFACE: &str = "org.freedesktop.NetworkManager.IP4Config";

/// Global NetworkManager connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NmState {
    #[default]
    Unknown = 0,
    Asleep = 10,
    Disconnected = 20,
    Disconnecting = 30,
    Connecting = 40,
    ConnectedLocal = 50,
    ConnectedSite = 60,
    ConnectedGlobal = 70,
}

impl NmState {
    /// Decodes NetworkManager numeric `NM_STATE_*` enum values.
    #[must_use]
    pub fn from_u32(val: u32) -> Self {
        match val {
            10 => Self::Asleep,
            20 => Self::Disconnected,
            30 => Self::Disconnecting,
            40 => Self::Connecting,
            50 => Self::ConnectedLocal,
            60 => Self::ConnectedSite,
            70 => Self::ConnectedGlobal,
            _ => Self::Unknown,
        }
    }
}

/// NetworkManager captive-portal / internet connectivity state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NmConnectivity {
    #[default]
    Unknown = 0,
    None = 1,
    Portal = 2,
    Limited = 3,
    Full = 4,
}

impl NmConnectivity {
    /// Decodes NetworkManager numeric `NM_CONNECTIVITY_*` enum values.
    #[must_use]
    pub fn from_u32(val: u32) -> Self {
        match val {
            1 => Self::None,
            2 => Self::Portal,
            3 => Self::Limited,
            4 => Self::Full,
            _ => Self::Unknown,
        }
    }
}

/// NetworkManager device kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NmDeviceType {
    #[default]
    Unknown,
    Ethernet,
    Wifi,
    Modem,
    Bluetooth,
    Bridge,
    Tun,
    WireGuard,
    Other(u32),
}

impl NmDeviceType {
    /// Decodes NetworkManager numeric `NM_DEVICE_TYPE_*` constants.
    #[must_use]
    pub fn from_u32(val: u32) -> Self {
        match val {
            0 => Self::Unknown,
            1 => Self::Ethernet,
            2 => Self::Wifi,
            5 => Self::Bluetooth,
            8 => Self::Modem,
            13 => Self::Bridge,
            16 => Self::Tun,
            29 => Self::WireGuard,
            other => Self::Other(other),
        }
    }
}

/// Visible Wi-Fi access point entry reported by NetworkManager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NmAccessPoint {
    pub ssid: String,
    pub strength: u8,
    pub frequency_mhz: u32,
    pub secured: bool,
    pub active: bool,
    pub hw_address: String,
    pub object_path: String,
}

/// Summary of a NetworkManager device interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NmDeviceInfo {
    pub object_path: String,
    pub interface: String,
    pub device_type: NmDeviceType,
    pub state: u32,
    pub ip4_address: Option<String>,
    pub active_ssid: Option<String>,
    pub strength: Option<u8>,
    pub bitrate_kbps: Option<u32>,
}

/// Active NetworkManager connection entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NmActiveConnection {
    pub id: String,
    pub conn_type: String,
    pub state: u32,
    pub devices: Vec<String>,
    pub default_v4: bool,
    pub default_v6: bool,
}

/// Snapshot of system NetworkManager connectivity, radios, devices, and active connections.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetworkManagerSnapshot {
    pub state: NmState,
    pub connectivity: NmConnectivity,
    pub networking_enabled: bool,
    pub wireless_enabled: bool,
    pub wireless_hardware_enabled: bool,
    pub primary_connection_type: Option<String>,
    pub primary_connection_id: Option<String>,
    pub devices: Vec<NmDeviceInfo>,
    pub active_connections: Vec<NmActiveConnection>,
}

impl Dbus {
    /// Subscribes to NetworkManager state and device changes on the system D-Bus.
    pub fn network_manager<M: 'static>(
        map: impl Fn(NetworkManagerSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusNetworkManager {
                key: "dbus:network-manager".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Reads the current [`NetworkManagerSnapshot`] from the system bus.
    pub fn network_manager_snapshot(&self) -> Result<NetworkManagerSnapshot> {
        let conn = self.connection(DbusBus::System)?;
        read_nm_snapshot_blocking(&conn)
    }

    /// Enables or disables Wi-Fi radios via `org.freedesktop.NetworkManager.WirelessEnabled`.
    pub fn nm_set_wireless_enabled(&self, enabled: bool) -> Result<()> {
        self.set_property(
            DbusBus::System,
            NM_DEST,
            NM_PATH,
            NM_IFACE,
            "WirelessEnabled",
            Value::from(enabled),
        )
    }

    /// Enables or disables all NetworkManager networking via `org.freedesktop.NetworkManager.Enable`.
    pub fn nm_set_networking_enabled(&self, enabled: bool) -> Result<()> {
        let conn = self.connection(DbusBus::System)?;
        conn.call_method(
            Some(NM_DEST),
            NM_PATH,
            Some(NM_IFACE),
            "Enable",
            &(enabled,),
        )
        .map_err(|e| WaywiError::DbusError(format!("NetworkManager.Enable failed: {e}")))?;
        Ok(())
    }

    /// Triggers an asynchronous Wi-Fi access-point scan on all wireless devices.
    pub fn nm_scan_wifi(&self) -> Result<()> {
        let conn = self.connection(DbusBus::System)?;
        let root_props = get_all_blocking(&conn, NM_DEST, NM_PATH, NM_IFACE)?;
        let device_paths = dict_str_vec(&root_props, "Devices");
        let empty_opts: HashMap<&str, Value<'_>> = HashMap::new();
        for path in device_paths.into_iter().take(32) {
            if let Ok(dev_props) = get_all_blocking(&conn, NM_DEST, &path, NM_DEVICE_IFACE)
                && dict_u32(&dev_props, "DeviceType") == Some(2)
            {
                conn.call_method(
                    Some(NM_DEST),
                    path.as_str(),
                    Some(NM_WIRELESS_IFACE),
                    "RequestScan",
                    &(empty_opts.clone(),),
                )
                .map_err(|e| WaywiError::DbusError(format!("RequestScan failed: {e}")))?;
            }
        }
        Ok(())
    }

    /// Enumerates visible Wi-Fi access points across NetworkManager wireless devices.
    pub fn nm_list_wifi_access_points(&self) -> Result<Vec<NmAccessPoint>> {
        let conn = self.connection(DbusBus::System)?;
        let root_props = get_all_blocking(&conn, NM_DEST, NM_PATH, NM_IFACE)?;
        let device_paths = dict_str_vec(&root_props, "Devices");
        let mut by_ssid: HashMap<String, NmAccessPoint> = HashMap::new();

        for dev_path in device_paths.into_iter().take(32) {
            let Ok(dev_props) = get_all_blocking(&conn, NM_DEST, &dev_path, NM_DEVICE_IFACE) else {
                continue;
            };
            if dict_u32(&dev_props, "DeviceType") != Some(2) {
                continue;
            }
            let Ok(wifi_props) = get_all_blocking(&conn, NM_DEST, &dev_path, NM_WIRELESS_IFACE)
            else {
                continue;
            };
            let active_ap = dict_object_path(&wifi_props, "ActiveAccessPoint");
            let ap_paths = dict_str_vec(&wifi_props, "AccessPoints");

            for ap_path in ap_paths.into_iter().take(128) {
                let Ok(ap_props) = get_all_blocking(&conn, NM_DEST, &ap_path, NM_AP_IFACE) else {
                    continue;
                };
                let ssid = dict_bytes(&ap_props, "Ssid")
                    .map(|b| parse_ssid_bytes(&b))
                    .unwrap_or_default();
                if ssid.is_empty() {
                    continue;
                }
                let strength = dict_u8(&ap_props, "Strength").unwrap_or(0);
                let frequency_mhz = dict_u32(&ap_props, "Frequency").unwrap_or(0);
                let flags = dict_u32(&ap_props, "Flags").unwrap_or(0);
                let wpa_flags = dict_u32(&ap_props, "WpaFlags").unwrap_or(0);
                let rsn_flags = dict_u32(&ap_props, "RsnFlags").unwrap_or(0);
                let hw_address = dict_str(&ap_props, "HwAddress").unwrap_or_default();
                let active = active_ap.as_deref() == Some(ap_path.as_str());

                let candidate = NmAccessPoint {
                    ssid: ssid.clone(),
                    strength,
                    frequency_mhz,
                    secured: is_ap_secured(flags, wpa_flags, rsn_flags),
                    active,
                    hw_address,
                    object_path: ap_path,
                };
                match by_ssid.get(&ssid) {
                    Some(prev)
                        if !candidate.active
                            && (prev.active || prev.strength >= candidate.strength) => {}
                    _ => {
                        by_ssid.insert(ssid, candidate);
                    }
                }
            }
        }

        let mut aps: Vec<NmAccessPoint> = by_ssid.into_values().collect();
        aps.sort_by(|a, b| {
            b.active
                .cmp(&a.active)
                .then_with(|| b.strength.cmp(&a.strength))
                .then_with(|| a.ssid.cmp(&b.ssid))
        });
        Ok(aps)
    }
}

fn parse_active_connection(ap: &HashMap<String, OwnedValue>) -> NmActiveConnection {
    NmActiveConnection {
        id: dict_str(ap, "Id").unwrap_or_default(),
        conn_type: dict_str(ap, "Type").unwrap_or_default(),
        state: dict_u32(ap, "State").unwrap_or(0),
        devices: dict_str_vec(ap, "Devices"),
        default_v4: dict_bool(ap, "Default").unwrap_or(false),
        default_v6: dict_bool(ap, "Default6").unwrap_or(false),
    }
}

fn parse_wifi_metrics(
    wifi: &HashMap<String, OwnedValue>,
    ap_info: Option<&HashMap<String, OwnedValue>>,
) -> (Option<String>, Option<u8>, Option<u32>) {
    let bitrate = dict_u32(wifi, "Bitrate");
    let ssid = ap_info
        .and_then(|ap| dict_bytes(ap, "Ssid"))
        .map(|b| parse_ssid_bytes(&b))
        .filter(|s| !s.is_empty());
    let strength = ap_info.and_then(|ap| dict_u8(ap, "Strength"));
    (ssid, strength, bitrate)
}

fn parse_device_info(
    object_path: String,
    dev: &HashMap<String, OwnedValue>,
    ip4_address: Option<String>,
    wifi_metrics: (Option<String>, Option<u8>, Option<u32>),
) -> NmDeviceInfo {
    let (active_ssid, strength, bitrate_kbps) = wifi_metrics;
    NmDeviceInfo {
        object_path,
        interface: dict_str(dev, "Interface").unwrap_or_default(),
        device_type: NmDeviceType::from_u32(dict_u32(dev, "DeviceType").unwrap_or(0)),
        state: dict_u32(dev, "State").unwrap_or(0),
        ip4_address,
        active_ssid,
        strength,
        bitrate_kbps,
    }
}

fn build_nm_snapshot(
    root: &HashMap<String, OwnedValue>,
    primary_id: Option<String>,
    active_connections: Vec<NmActiveConnection>,
    devices: Vec<NmDeviceInfo>,
) -> NetworkManagerSnapshot {
    NetworkManagerSnapshot {
        state: NmState::from_u32(dict_u32(root, "State").unwrap_or(0)),
        connectivity: NmConnectivity::from_u32(dict_u32(root, "Connectivity").unwrap_or(0)),
        networking_enabled: dict_bool(root, "NetworkingEnabled").unwrap_or(false),
        wireless_enabled: dict_bool(root, "WirelessEnabled").unwrap_or(false),
        wireless_hardware_enabled: dict_bool(root, "WirelessHardwareEnabled").unwrap_or(false),
        primary_connection_type: dict_str(root, "PrimaryConnectionType").filter(|s| !s.is_empty()),
        primary_connection_id: primary_id,
        devices,
        active_connections,
    }
}

fn read_nm_snapshot_blocking(conn: &zbus::blocking::Connection) -> Result<NetworkManagerSnapshot> {
    let root = get_all_blocking(conn, NM_DEST, NM_PATH, NM_IFACE)?;
    let primary_path = dict_object_path(&root, "PrimaryConnection");

    let mut primary_id = None;
    let mut active_connections = Vec::new();
    for path in dict_str_vec(&root, "ActiveConnections")
        .into_iter()
        .take(64)
    {
        let Ok(active_conn) = get_all_blocking(conn, NM_DEST, &path, NM_ACTIVE_IFACE) else {
            continue;
        };
        let conn_info = parse_active_connection(&active_conn);
        if primary_path.as_deref() == Some(path.as_str()) && !conn_info.id.is_empty() {
            primary_id = Some(conn_info.id.clone());
        }
        active_connections.push(conn_info);
    }

    let mut devices = Vec::new();
    for dev_path in dict_str_vec(&root, "Devices").into_iter().take(64) {
        let Ok(dev) = get_all_blocking(conn, NM_DEST, &dev_path, NM_DEVICE_IFACE) else {
            continue;
        };
        let device_type = NmDeviceType::from_u32(dict_u32(&dev, "DeviceType").unwrap_or(0));
        let ip4_address = dict_object_path(&dev, "Ip4Config")
            .and_then(|ip_path| get_all_blocking(conn, NM_DEST, &ip_path, NM_IP4_IFACE).ok())
            .and_then(|ip_props| extract_first_ipv4(ip_props.get("AddressData")));

        let wifi_metrics = if device_type == NmDeviceType::Wifi {
            if let Ok(wifi) = get_all_blocking(conn, NM_DEST, &dev_path, NM_WIRELESS_IFACE) {
                let ap_info = dict_object_path(&wifi, "ActiveAccessPoint")
                    .and_then(|ap| get_all_blocking(conn, NM_DEST, &ap, NM_AP_IFACE).ok());
                parse_wifi_metrics(&wifi, ap_info.as_ref())
            } else {
                (None, None, None)
            }
        } else {
            (None, None, None)
        };

        devices.push(parse_device_info(dev_path, &dev, ip4_address, wifi_metrics));
    }

    Ok(build_nm_snapshot(
        &root,
        primary_id,
        active_connections,
        devices,
    ))
}

async fn read_nm_snapshot_async(conn: &zbus::Connection) -> Option<NetworkManagerSnapshot> {
    let root = get_all_async(conn, NM_DEST, NM_PATH, NM_IFACE).await?;
    let primary_path = dict_object_path(&root, "PrimaryConnection");

    let mut primary_id = None;
    let mut active_connections = Vec::new();
    for path in dict_str_vec(&root, "ActiveConnections")
        .into_iter()
        .take(64)
    {
        let Some(active_conn) = get_all_async(conn, NM_DEST, &path, NM_ACTIVE_IFACE).await else {
            continue;
        };
        let conn_info = parse_active_connection(&active_conn);
        if primary_path.as_deref() == Some(path.as_str()) && !conn_info.id.is_empty() {
            primary_id = Some(conn_info.id.clone());
        }
        active_connections.push(conn_info);
    }

    let mut devices = Vec::new();
    for dev_path in dict_str_vec(&root, "Devices").into_iter().take(64) {
        let Some(dev) = get_all_async(conn, NM_DEST, &dev_path, NM_DEVICE_IFACE).await else {
            continue;
        };
        let device_type = NmDeviceType::from_u32(dict_u32(&dev, "DeviceType").unwrap_or(0));
        let ip4_address = if let Some(ip_path) = dict_object_path(&dev, "Ip4Config") {
            get_all_async(conn, NM_DEST, &ip_path, NM_IP4_IFACE)
                .await
                .and_then(|ip_props| extract_first_ipv4(ip_props.get("AddressData")))
        } else {
            None
        };

        let wifi_metrics = if device_type == NmDeviceType::Wifi {
            if let Some(wifi) = get_all_async(conn, NM_DEST, &dev_path, NM_WIRELESS_IFACE).await {
                let ap_info = if let Some(ap_path) = dict_object_path(&wifi, "ActiveAccessPoint") {
                    get_all_async(conn, NM_DEST, &ap_path, NM_AP_IFACE).await
                } else {
                    None
                };
                parse_wifi_metrics(&wifi, ap_info.as_ref())
            } else {
                (None, None, None)
            }
        } else {
            (None, None, None)
        };

        devices.push(parse_device_info(dev_path, &dev, ip4_address, wifi_metrics));
    }

    Some(build_nm_snapshot(
        &root,
        primary_id,
        active_connections,
        devices,
    ))
}

pub(crate) fn parse_ssid_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn is_ap_secured(flags: u32, wpa_flags: u32, rsn_flags: u32) -> bool {
    (flags & 0x1) != 0 || wpa_flags != 0 || rsn_flags != 0
}

fn extract_first_ipv4(val: Option<&OwnedValue>) -> Option<String> {
    let entries = Vec::<HashMap<String, OwnedValue>>::try_from(val?.clone()).ok()?;
    for entry in entries {
        if let Some(addr) = dict_str(&entry, "address")
            && !addr.is_empty()
        {
            return Some(addr);
        }
    }
    None
}

pub(crate) struct NetworkManagerWorker {
    stop: AsyncStopSignal,
    join: Option<std::thread::JoinHandle<()>>,
}

impl NetworkManagerWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<NetworkManagerSnapshot>,
        shared: DbusSharedBus,
    ) -> Self {
        let stop = AsyncStopSignal::new();
        let stop_sig = stop.clone();
        let join = std::thread::Builder::new()
            .name("waywi-dbus-nm".to_owned())
            .spawn(move || {
                let Ok(conn) = connect_shared_bus(DbusBus::System, &shared) else {
                    return;
                };
                futures_lite::future::block_on(async move {
                    if let Some(snap) = read_nm_snapshot_async(&conn).await {
                        let _ = sender.send(snap);
                    }
                    let Ok(rule) = zbus::MatchRule::builder()
                        .msg_type(zbus::message::Type::Signal)
                        .sender(NM_DEST)
                    else {
                        return;
                    };
                    let Ok(mut stream) =
                        zbus::MessageStream::for_match_rule(rule.build(), &conn, Some(64)).await
                    else {
                        return;
                    };
                    use futures_lite::StreamExt;
                    while !stop_sig.is_stopped() {
                        let next_msg =
                            futures_lite::future::or(async { Some(stream.next().await) }, async {
                                stop_sig.wait_stopped().await;
                                None
                            })
                            .await;
                        let Some(Some(Ok(_))) = next_msg else {
                            break;
                        };
                        if let Some(snap) = read_nm_snapshot_async(&conn).await {
                            let _ = sender.send(snap);
                        }
                    }
                });
            })
            .ok();
        Self { stop, join }
    }
}

impl Drop for NetworkManagerWorker {
    fn drop(&mut self) {
        self.stop.trigger();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_nm_enums_ssid_and_security() {
        assert_eq!(NmState::from_u32(70), NmState::ConnectedGlobal);
        assert_eq!(NmConnectivity::from_u32(4), NmConnectivity::Full);
        assert_eq!(NmDeviceType::from_u32(2), NmDeviceType::Wifi);
        assert_eq!(parse_ssid_bytes(b"HomeWiFi_5G"), "HomeWiFi_5G");
        assert!(!is_ap_secured(0, 0, 0));
        assert!(is_ap_secured(0, 0x100, 0));
    }
}
