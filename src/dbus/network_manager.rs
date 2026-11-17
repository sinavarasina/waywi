//! System NetworkManager inspection, Wi-Fi management, and connection control (`org.freedesktop.NetworkManager`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{
    DBUS_SLOW_TIMEOUT, DbusActionLane, DbusActionPayload, DbusClient, DbusSharedBus,
    LaneActionSpec, connect_shared_bus, validate_object_path,
};
use super::types::{Dbus, DbusBus};
use super::worker::AsyncStopSignal;
use super::zvariant_helpers::{
    dict_bool, dict_bytes, dict_object_path, dict_str, dict_str_vec, dict_u8, dict_u32,
    get_all_async, get_all_blocking, unwrap_value,
};

const NM_DEST: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const NM_SETTINGS_IFACE: &str = "org.freedesktop.NetworkManager.Settings";
const NM_SETTINGS_CONN_IFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const NM_DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const NM_WIRELESS_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const NM_AP_IFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
const NM_ACTIVE_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const NM_IP4_IFACE: &str = "org.freedesktop.NetworkManager.IP4Config";

/// Minimum interval between expensive Wi-Fi AP / Settings refreshes on `PropertiesChanged` bursts.
const NM_WIFI_REFRESH_COOLDOWN: Duration = Duration::from_secs(2);

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

/// Wi-Fi access point security classification decoded from `Flags`, `WpaFlags`, and `RsnFlags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NmWifiSecurity {
    #[default]
    Open,
    Wep,
    WpaPsk,
    WpaEap,
    Wpa3Sae,
    Owe,
}

/// Visible Wi-Fi access point entry reported by NetworkManager.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NmAccessPoint {
    pub ssid: String,
    pub strength: u8,
    pub frequency_mhz: u32,
    pub max_bitrate_kbps: u32,
    pub secured: bool,
    pub security: NmWifiSecurity,
    pub active: bool,
    pub known: bool,
    pub saved_connection_path: Option<String>,
    pub device_path: String,
    pub hw_address: String,
    pub object_path: String,
}

/// Saved NetworkManager connection profile (`org.freedesktop.NetworkManager.Settings.Connection`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NmSavedConnection {
    pub object_path: String,
    pub id: String,
    pub uuid: String,
    pub conn_type: String,
    pub ssid: Option<String>,
    pub autoconnect: bool,
    pub timestamp: u64,
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NmActiveConnection {
    pub object_path: String,
    pub connection_path: Option<String>,
    pub id: String,
    pub uuid: String,
    pub conn_type: String,
    pub state: u32,
    pub devices: Vec<String>,
    pub default_v4: bool,
    pub default_v6: bool,
}

/// Snapshot of system NetworkManager connectivity, radios, devices, active connections, and Wi-Fi access points.
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
    pub access_points: Vec<NmAccessPoint>,
    pub saved_connections: Vec<NmSavedConnection>,
}

impl Dbus {
    /// Subscribes to NetworkManager state, device, and Wi-Fi access-point changes on the system D-Bus.
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
    /// Reads the current [`NetworkManagerSnapshot`] from the in-memory subscription cache (0 ms)
    /// or queries the system bus if not yet cached.
    pub fn network_manager_snapshot(&self) -> Result<NetworkManagerSnapshot> {
        if let Ok(guard) = self.cache.network_manager.read()
            && let Some(ref snap) = *guard
        {
            return Ok(snap.clone());
        }
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

    /// Enables or disables all NetworkManager networking via `org.freedesktop.NetworkManager.Enable`
    /// on the NetworkManager lane.
    pub fn nm_set_networking_enabled(&self, enabled: bool) -> Result<()> {
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::NetworkManager,
                DbusBus::System,
                "NetworkManager.Enable",
                NM_PATH,
            ),
            move |conn| async move {
                conn.call_method(
                    Some(NM_DEST),
                    NM_PATH,
                    Some(NM_IFACE),
                    "Enable",
                    &(enabled,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("NetworkManager.Enable failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Triggers an asynchronous Wi-Fi access-point scan on all wireless devices on the
    /// NetworkManager lane.
    pub fn nm_scan_wifi(&self) -> Result<()> {
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::NetworkManager,
                DbusBus::System,
                "Wireless.RequestScan",
                NM_PATH,
            )
            .with_dedup("nm:scan-wifi"),
            move |conn| async move {
                let Some(root_props) = get_all_async(&conn, NM_DEST, NM_PATH, NM_IFACE).await
                else {
                    return Err(WaywiError::DbusError(
                        "failed to read NetworkManager root properties".into(),
                    ));
                };
                let device_paths = dict_str_vec(&root_props, "Devices");
                let mut last_err = None;
                for path in device_paths.into_iter().take(32) {
                    if let Some(dev_props) =
                        get_all_async(&conn, NM_DEST, &path, NM_DEVICE_IFACE).await
                        && dict_u32(&dev_props, "DeviceType") == Some(2)
                    {
                        let empty_opts: HashMap<&str, Value<'_>> = HashMap::new();
                        if let Err(e) = conn
                            .call_method(
                                Some(NM_DEST),
                                path.as_str(),
                                Some(NM_WIRELESS_IFACE),
                                "RequestScan",
                                &(empty_opts,),
                            )
                            .await
                        {
                            last_err =
                                Some(WaywiError::DbusError(format!("RequestScan failed: {e}")));
                        }
                    }
                }
                if let Some(err) = last_err {
                    return Err(err);
                }
                Ok(())
            },
        )
    }

    /// Enumerates visible Wi-Fi access points across NetworkManager wireless devices, returning
    /// the cached subscription list (0 ms) when available or querying the system bus.
    pub fn nm_list_wifi_access_points(&self) -> Result<Vec<NmAccessPoint>> {
        if let Ok(guard) = self.cache.network_manager.read()
            && let Some(ref snap) = *guard
        {
            return Ok(snap.access_points.clone());
        }
        let conn = self.connection(DbusBus::System)?;
        let root_props = get_all_blocking(&conn, NM_DEST, NM_PATH, NM_IFACE)?;
        let device_paths = dict_str_vec(&root_props, "Devices");
        let saved = list_saved_connections_blocking(&conn).unwrap_or_default();
        Ok(collect_wifi_access_points_blocking(
            &conn,
            &device_paths,
            &saved,
        ))
    }

    /// Lists saved connection profiles from `org.freedesktop.NetworkManager.Settings`, returning
    /// the cached subscription list (0 ms) when available or querying the system bus.
    pub fn nm_list_saved_connections(&self) -> Result<Vec<NmSavedConnection>> {
        if let Ok(guard) = self.cache.network_manager.read()
            && let Some(ref snap) = *guard
        {
            return Ok(snap.saved_connections.clone());
        }
        let conn = self.connection(DbusBus::System)?;
        list_saved_connections_blocking(&conn)
    }

    /// Connects to a visible Wi-Fi [`NmAccessPoint`] on the NetworkManager lane without locking
    /// the profile to a single BSSID (allowing seamless roaming across mesh / multi-AP networks).
    ///
    /// - If `password` is `None` and the access point matches an existing saved profile, activates
    ///   that saved connection via `ActivateConnection`.
    /// - Otherwise creates and activates a new connection profile via `AddAndActivateConnection`
    ///   using `ap.security` (`Open`, `Owe`, `Wep`, `WpaPsk`, `Wpa3Sae`).
    pub fn nm_connect_wifi(&self, ap: &NmAccessPoint, password: Option<&str>) -> Result<()> {
        self.nm_connect_wifi_with_options(ap, password, false)
    }

    /// Connects to a visible Wi-Fi [`NmAccessPoint`] on the NetworkManager lane, optionally
    /// pinning activation to `ap.object_path` when `lock_bssid` is `true`.
    pub fn nm_connect_wifi_with_options(
        &self,
        ap: &NmAccessPoint,
        password: Option<&str>,
        lock_bssid: bool,
    ) -> Result<()> {
        let specific_ap = if lock_bssid && !ap.object_path.is_empty() {
            validate_object_path(&ap.object_path, "access point path")?
        } else {
            "/".to_owned()
        };

        if password.is_none()
            && let Some(ref saved_path) = ap.saved_connection_path
        {
            return self.nm_activate_connection(saved_path, &ap.device_path, Some(&specific_ap));
        }

        let ssid = ap.ssid.clone();
        let target = ssid.clone();
        let security = ap.security;
        let pw_owned = password.map(str::to_owned);
        let dev_hint = ap.device_path.clone();
        let dedup = format!("nm:connect:{ssid}");

        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::NetworkManager,
                DbusBus::System,
                "NetworkManager.AddAndActivateConnection",
                target,
            )
            .with_dedup(dedup)
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let dev_path = if !dev_hint.is_empty() {
                    dev_hint
                } else {
                    find_first_wifi_device_async(&conn).await.ok_or_else(|| {
                        WaywiError::DbusError("no Wi-Fi device managed by NetworkManager".into())
                    })?
                };
                let dev_obj = ObjectPath::try_from(dev_path.as_str())
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                let ap_obj = ObjectPath::try_from(specific_ap.as_str())
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                let settings =
                    build_wifi_connection_settings(&ssid, security, pw_owned.as_deref(), false);
                let reply = conn
                    .call_method(
                        Some(NM_DEST),
                        NM_PATH,
                        Some(NM_IFACE),
                        "AddAndActivateConnection",
                        &(settings, dev_obj, ap_obj),
                    )
                    .await
                    .map_err(|e| {
                        WaywiError::DbusError(format!(
                            "AddAndActivateConnection for `{ssid}` failed: {e}"
                        ))
                    })?;
                let (_profile_path, active_path): (OwnedObjectPath, OwnedObjectPath) = reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                Ok(DbusActionPayload::with_object_path(active_path.to_string()))
            },
        )
    }

    /// Connects to a hidden Wi-Fi network by `ssid` and optional `password` on the NetworkManager lane.
    pub fn nm_connect_hidden_wifi(
        &self,
        ssid: &str,
        password: Option<&str>,
        device_path: Option<&str>,
    ) -> Result<()> {
        let security = if password.is_some_and(|p| !p.is_empty()) {
            NmWifiSecurity::WpaPsk
        } else {
            NmWifiSecurity::Open
        };
        self.nm_connect_hidden_wifi_with_security(ssid, security, password, device_path)
    }

    /// Connects to a hidden Wi-Fi network with an explicit [`NmWifiSecurity`] mode (`Open`, `Owe`,
    /// `WpaPsk`, `Wpa3Sae`, `Wep`) on the NetworkManager lane.
    pub fn nm_connect_hidden_wifi_with_security(
        &self,
        ssid: &str,
        security: NmWifiSecurity,
        password: Option<&str>,
        device_path: Option<&str>,
    ) -> Result<()> {
        if ssid.is_empty() {
            return Err(WaywiError::DbusError("Wi-Fi SSID cannot be empty".into()));
        }
        let ssid_owned = ssid.to_owned();
        let target = ssid_owned.clone();
        let pw_owned = password.map(str::to_owned);
        let dev_hint = device_path.unwrap_or_default().to_owned();
        let dedup = format!("nm:connect-hidden:{ssid_owned}");

        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::NetworkManager,
                DbusBus::System,
                "NetworkManager.AddAndActivateConnection",
                target,
            )
            .with_dedup(dedup)
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let dev_path = if !dev_hint.is_empty() {
                    dev_hint
                } else {
                    find_first_wifi_device_async(&conn).await.ok_or_else(|| {
                        WaywiError::DbusError("no Wi-Fi device managed by NetworkManager".into())
                    })?
                };
                let dev_obj = ObjectPath::try_from(dev_path.as_str())
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                let root_obj = ObjectPath::from_static_str("/")
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                let settings = build_wifi_connection_settings(
                    &ssid_owned,
                    security,
                    pw_owned.as_deref(),
                    true,
                );
                let reply = conn
                    .call_method(
                        Some(NM_DEST),
                        NM_PATH,
                        Some(NM_IFACE),
                        "AddAndActivateConnection",
                        &(settings, dev_obj, root_obj),
                    )
                    .await
                    .map_err(|e| {
                        WaywiError::DbusError(format!(
                            "hidden Wi-Fi AddAndActivateConnection for `{ssid_owned}` failed: {e}"
                        ))
                    })?;
                let (_profile_path, active_path): (OwnedObjectPath, OwnedObjectPath) = reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                Ok(DbusActionPayload::with_object_path(active_path.to_string()))
            },
        )
    }

    /// Activates an existing saved NetworkManager connection profile (`ActivateConnection`) on the
    /// NetworkManager lane.
    pub fn nm_activate_connection(
        &self,
        connection_path: &str,
        device_path: &str,
        specific_object: Option<&str>,
    ) -> Result<()> {
        let conn_obj = OwnedObjectPath::try_from(connection_path.to_owned())
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        let spec_str = specific_object.filter(|s| !s.is_empty()).unwrap_or("/");
        let spec_obj = OwnedObjectPath::try_from(spec_str.to_owned())
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        let dev_hint = device_path.to_owned();
        let target = connection_path.to_owned();
        let dedup = format!("nm:activate:{connection_path}");

        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::NetworkManager,
                DbusBus::System,
                "NetworkManager.ActivateConnection",
                target,
            )
            .with_dedup(dedup)
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let resolved_dev = if !dev_hint.is_empty() {
                    dev_hint
                } else {
                    find_first_wifi_device_async(&conn)
                        .await
                        .unwrap_or_else(|| "/".to_owned())
                };
                let dev_obj = ObjectPath::try_from(resolved_dev.as_str())
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                let reply = conn
                    .call_method(
                        Some(NM_DEST),
                        NM_PATH,
                        Some(NM_IFACE),
                        "ActivateConnection",
                        &(conn_obj, dev_obj, spec_obj),
                    )
                    .await
                    .map_err(|e| {
                        WaywiError::DbusError(format!(
                            "NetworkManager.ActivateConnection failed: {e}"
                        ))
                    })?;
                let active_path: OwnedObjectPath = reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                Ok(DbusActionPayload::with_object_path(active_path.to_string()))
            },
        )
    }

    /// Deactivates an active connection by its `ActiveConnection` object path (`DeactivateConnection`)
    /// on the NetworkManager lane.
    pub fn nm_deactivate_connection(&self, active_connection_path: &str) -> Result<()> {
        let obj = OwnedObjectPath::try_from(active_connection_path.to_owned())
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        let target = active_connection_path.to_owned();
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::NetworkManager,
                DbusBus::System,
                "NetworkManager.DeactivateConnection",
                target,
            ),
            move |conn| async move {
                conn.call_method(
                    Some(NM_DEST),
                    NM_PATH,
                    Some(NM_IFACE),
                    "DeactivateConnection",
                    &(obj,),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!(
                        "NetworkManager.DeactivateConnection failed: {e}"
                    ))
                })?;
                Ok(())
            },
        )
    }

    /// Disconnects a specific NetworkManager device (`org.freedesktop.NetworkManager.Device.Disconnect`)
    /// on the NetworkManager lane.
    pub fn nm_disconnect_device(&self, device_path: &str) -> Result<()> {
        self.call_lane_method_unit(
            DbusActionLane::NetworkManager,
            DbusBus::System,
            NM_DEST,
            device_path,
            NM_DEVICE_IFACE,
            "Disconnect",
        )
    }

    /// Deletes/forgets a saved connection profile by its settings object path
    /// (`org.freedesktop.NetworkManager.Settings.Connection.Delete`) on the NetworkManager lane.
    pub fn nm_forget_connection(&self, connection_path: &str) -> Result<()> {
        self.call_lane_method_unit(
            DbusActionLane::NetworkManager,
            DbusBus::System,
            NM_DEST,
            connection_path,
            NM_SETTINGS_CONN_IFACE,
            "Delete",
        )
    }

    /// Deletes/forgets all saved Wi-Fi connection profiles matching `ssid` on the NetworkManager
    /// lane.
    pub fn nm_forget_wifi_ssid(&self, ssid: &str) -> Result<()> {
        if ssid.is_empty() {
            return Err(WaywiError::DbusError("Wi-Fi SSID cannot be empty".into()));
        }
        let target_ssid = ssid.to_owned();
        let target = target_ssid.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::NetworkManager,
                DbusBus::System,
                "Settings.ForgetWifiSsid",
                target,
            ),
            move |conn| async move {
                let saved = list_saved_connections_async(&conn).await;
                let mut deleted = 0usize;
                for item in saved {
                    if item.ssid.as_deref() == Some(target_ssid.as_str()) {
                        conn.call_method(
                            Some(NM_DEST),
                            item.object_path.as_str(),
                            Some(NM_SETTINGS_CONN_IFACE),
                            "Delete",
                            &(),
                        )
                        .await
                        .map_err(|e| {
                            WaywiError::DbusError(format!(
                                "Settings.Connection.Delete failed for `{}`: {e}",
                                item.object_path
                            ))
                        })?;
                        deleted += 1;
                    }
                }
                Ok(DbusActionPayload::with_count(deleted))
            },
        )
    }
}

pub(crate) fn build_wifi_connection_settings<'a>(
    ssid: &'a str,
    security: NmWifiSecurity,
    password: Option<&'a str>,
    hidden: bool,
) -> HashMap<&'static str, HashMap<&'static str, Value<'a>>> {
    let mut conn_sec: HashMap<&'static str, Value<'a>> = HashMap::new();
    conn_sec.insert("id", Value::from(ssid));
    conn_sec.insert("type", Value::from("802-11-wireless"));
    conn_sec.insert("autoconnect", Value::from(true));

    let mut wifi_sec: HashMap<&'static str, Value<'a>> = HashMap::new();
    wifi_sec.insert("ssid", Value::from(ssid.as_bytes().to_vec()));
    wifi_sec.insert("mode", Value::from("infrastructure"));
    if hidden {
        wifi_sec.insert("hidden", Value::from(true));
    }

    let mut top: HashMap<&'static str, HashMap<&'static str, Value<'a>>> = HashMap::new();
    top.insert("connection", conn_sec);
    top.insert("802-11-wireless", wifi_sec);

    let clean_pw = password.filter(|p| !p.is_empty());
    match (security, clean_pw) {
        (NmWifiSecurity::Owe, _) => {
            let mut sec: HashMap<&'static str, Value<'a>> = HashMap::new();
            sec.insert("key-mgmt", Value::from("owe"));
            top.insert("802-11-wireless-security", sec);
        }
        (NmWifiSecurity::Wpa3Sae, Some(psk)) => {
            let mut sec: HashMap<&'static str, Value<'a>> = HashMap::new();
            sec.insert("key-mgmt", Value::from("sae"));
            sec.insert("psk", Value::from(psk));
            top.insert("802-11-wireless-security", sec);
        }
        (NmWifiSecurity::Wep, Some(key)) => {
            let mut sec: HashMap<&'static str, Value<'a>> = HashMap::new();
            sec.insert("key-mgmt", Value::from("none"));
            sec.insert("wep-key0", Value::from(key));
            top.insert("802-11-wireless-security", sec);
        }
        (_, Some(psk)) => {
            let mut sec: HashMap<&'static str, Value<'a>> = HashMap::new();
            sec.insert("key-mgmt", Value::from("wpa-psk"));
            sec.insert("psk", Value::from(psk));
            top.insert("802-11-wireless-security", sec);
        }
        _ => {}
    }

    top
}

async fn find_first_wifi_device_async(conn: &zbus::Connection) -> Option<String> {
    let root_props = get_all_async(conn, NM_DEST, NM_PATH, NM_IFACE).await?;
    for dev_path in dict_str_vec(&root_props, "Devices").into_iter().take(32) {
        if let Some(dev_props) = get_all_async(conn, NM_DEST, &dev_path, NM_DEVICE_IFACE).await
            && dict_u32(&dev_props, "DeviceType") == Some(2)
        {
            return Some(dev_path);
        }
    }
    None
}

fn list_saved_connections_blocking(
    conn: &zbus::blocking::Connection,
) -> Result<Vec<NmSavedConnection>> {
    let settings_props = get_all_blocking(conn, NM_DEST, NM_SETTINGS_PATH, NM_SETTINGS_IFACE)?;
    let paths = dict_str_vec(&settings_props, "Connections");
    let mut out = Vec::new();
    for path in paths.into_iter().take(256) {
        let Ok(reply) = conn.call_method(
            Some(NM_DEST),
            path.as_str(),
            Some(NM_SETTINGS_CONN_IFACE),
            "GetSettings",
            &(),
        ) else {
            continue;
        };
        let Ok(settings) = reply
            .body()
            .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
        else {
            continue;
        };
        if let Some(parsed) = parse_saved_connection(path, &settings) {
            out.push(parsed);
        }
    }
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

async fn list_saved_connections_async(conn: &zbus::Connection) -> Vec<NmSavedConnection> {
    let Some(settings_props) =
        get_all_async(conn, NM_DEST, NM_SETTINGS_PATH, NM_SETTINGS_IFACE).await
    else {
        return Vec::new();
    };
    let paths = dict_str_vec(&settings_props, "Connections");
    let mut out = Vec::new();
    for path in paths.into_iter().take(256) {
        let Ok(reply) = conn
            .call_method(
                Some(NM_DEST),
                path.as_str(),
                Some(NM_SETTINGS_CONN_IFACE),
                "GetSettings",
                &(),
            )
            .await
        else {
            continue;
        };
        let Ok(settings) = reply
            .body()
            .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
        else {
            continue;
        };
        if let Some(parsed) = parse_saved_connection(path, &settings) {
            out.push(parsed);
        }
    }
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.id.cmp(&b.id)));
    out
}

pub(crate) fn parse_saved_connection(
    object_path: String,
    settings: &HashMap<String, HashMap<String, OwnedValue>>,
) -> Option<NmSavedConnection> {
    let conn_sec = settings.get("connection")?;
    let id = dict_str(conn_sec, "id").unwrap_or_default();
    if id.is_empty() {
        return None;
    }
    let uuid = dict_str(conn_sec, "uuid").unwrap_or_default();
    let conn_type = dict_str(conn_sec, "type").unwrap_or_default();
    let autoconnect = dict_bool(conn_sec, "autoconnect").unwrap_or(true);
    let timestamp = match conn_sec.get("timestamp").map(|v| unwrap_value(v)) {
        Some(Value::U64(v)) => *v,
        Some(Value::U32(v)) => u64::from(*v),
        _ => 0,
    };
    let ssid = settings
        .get("802-11-wireless")
        .and_then(|w| dict_bytes(w, "ssid"))
        .map(|b| parse_ssid_bytes(&b))
        .filter(|s| !s.is_empty());

    Some(NmSavedConnection {
        object_path,
        id,
        uuid,
        conn_type,
        ssid,
        autoconnect,
        timestamp,
    })
}

fn collect_wifi_access_points_blocking(
    conn: &zbus::blocking::Connection,
    device_paths: &[String],
    saved_connections: &[NmSavedConnection],
) -> Vec<NmAccessPoint> {
    let mut by_ssid: HashMap<String, NmAccessPoint> = HashMap::new();

    for dev_path in device_paths.iter().take(32) {
        let Ok(dev_props) = get_all_blocking(conn, NM_DEST, dev_path, NM_DEVICE_IFACE) else {
            continue;
        };
        if dict_u32(&dev_props, "DeviceType") != Some(2) {
            continue;
        }
        let Ok(wifi_props) = get_all_blocking(conn, NM_DEST, dev_path, NM_WIRELESS_IFACE) else {
            continue;
        };
        let active_ap = dict_object_path(&wifi_props, "ActiveAccessPoint");
        let ap_paths = dict_str_vec(&wifi_props, "AccessPoints");

        for ap_path in ap_paths.into_iter().take(128) {
            let Ok(ap_props) = get_all_blocking(conn, NM_DEST, &ap_path, NM_AP_IFACE) else {
                continue;
            };
            if let Some(candidate) = build_access_point_entry(
                dev_path,
                ap_path,
                &ap_props,
                active_ap.as_deref(),
                saved_connections,
            ) {
                merge_access_point_candidate(&mut by_ssid, candidate);
            }
        }
    }

    finalize_access_points(by_ssid)
}

async fn collect_wifi_access_points_async(
    conn: &zbus::Connection,
    device_paths: &[String],
    saved_connections: &[NmSavedConnection],
) -> Vec<NmAccessPoint> {
    let mut by_ssid: HashMap<String, NmAccessPoint> = HashMap::new();

    for dev_path in device_paths.iter().take(32) {
        let Some(dev_props) = get_all_async(conn, NM_DEST, dev_path, NM_DEVICE_IFACE).await else {
            continue;
        };
        if dict_u32(&dev_props, "DeviceType") != Some(2) {
            continue;
        }
        let Some(wifi_props) = get_all_async(conn, NM_DEST, dev_path, NM_WIRELESS_IFACE).await
        else {
            continue;
        };
        let active_ap = dict_object_path(&wifi_props, "ActiveAccessPoint");
        let ap_paths = dict_str_vec(&wifi_props, "AccessPoints");

        for ap_path in ap_paths.into_iter().take(128) {
            let Some(ap_props) = get_all_async(conn, NM_DEST, &ap_path, NM_AP_IFACE).await else {
                continue;
            };
            if let Some(candidate) = build_access_point_entry(
                dev_path,
                ap_path,
                &ap_props,
                active_ap.as_deref(),
                saved_connections,
            ) {
                merge_access_point_candidate(&mut by_ssid, candidate);
            }
        }
    }

    finalize_access_points(by_ssid)
}

pub(crate) fn build_access_point_entry(
    dev_path: &str,
    ap_path: String,
    ap_props: &HashMap<String, OwnedValue>,
    active_ap: Option<&str>,
    saved_connections: &[NmSavedConnection],
) -> Option<NmAccessPoint> {
    let ssid = dict_bytes(ap_props, "Ssid")
        .map(|b| parse_ssid_bytes(&b))
        .unwrap_or_default();
    if ssid.is_empty() {
        return None;
    }
    let strength = dict_u8(ap_props, "Strength").unwrap_or(0);
    let frequency_mhz = dict_u32(ap_props, "Frequency").unwrap_or(0);
    let max_bitrate_kbps = dict_u32(ap_props, "MaxBitrate").unwrap_or(0);
    let flags = dict_u32(ap_props, "Flags").unwrap_or(0);
    let wpa_flags = dict_u32(ap_props, "WpaFlags").unwrap_or(0);
    let rsn_flags = dict_u32(ap_props, "RsnFlags").unwrap_or(0);
    let hw_address = dict_str(ap_props, "HwAddress").unwrap_or_default();
    let active = active_ap == Some(ap_path.as_str());
    let security = classify_ap_security(flags, wpa_flags, rsn_flags);
    let saved_connection_path = saved_connections
        .iter()
        .find(|c| c.ssid.as_deref() == Some(ssid.as_str()))
        .map(|c| c.object_path.clone());

    Some(NmAccessPoint {
        ssid,
        strength,
        frequency_mhz,
        max_bitrate_kbps,
        secured: is_ap_secured(flags, wpa_flags, rsn_flags),
        security,
        active,
        known: saved_connection_path.is_some(),
        saved_connection_path,
        device_path: dev_path.to_owned(),
        hw_address,
        object_path: ap_path,
    })
}

fn merge_access_point_candidate(
    by_ssid: &mut HashMap<String, NmAccessPoint>,
    candidate: NmAccessPoint,
) {
    match by_ssid.get(&candidate.ssid) {
        Some(prev) if !candidate.active && (prev.active || prev.strength >= candidate.strength) => {
        }
        _ => {
            by_ssid.insert(candidate.ssid.clone(), candidate);
        }
    }
}

fn finalize_access_points(by_ssid: HashMap<String, NmAccessPoint>) -> Vec<NmAccessPoint> {
    let mut aps: Vec<NmAccessPoint> = by_ssid.into_values().collect();
    aps.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| b.known.cmp(&a.known))
            .then_with(|| b.strength.cmp(&a.strength))
            .then_with(|| a.ssid.cmp(&b.ssid))
    });
    aps
}

fn parse_active_connection(
    object_path: String,
    ap: &HashMap<String, OwnedValue>,
) -> NmActiveConnection {
    NmActiveConnection {
        object_path,
        connection_path: dict_object_path(ap, "Connection"),
        id: dict_str(ap, "Id").unwrap_or_default(),
        uuid: dict_str(ap, "Uuid").unwrap_or_default(),
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
    access_points: Vec<NmAccessPoint>,
    saved_connections: Vec<NmSavedConnection>,
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
        access_points,
        saved_connections,
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
        let conn_info = parse_active_connection(path.clone(), &active_conn);
        if primary_path.as_deref() == Some(path.as_str()) && !conn_info.id.is_empty() {
            primary_id = Some(conn_info.id.clone());
        }
        active_connections.push(conn_info);
    }

    let device_paths: Vec<String> = dict_str_vec(&root, "Devices")
        .into_iter()
        .take(64)
        .collect();
    let mut devices = Vec::new();
    for dev_path in &device_paths {
        let Ok(dev) = get_all_blocking(conn, NM_DEST, dev_path, NM_DEVICE_IFACE) else {
            continue;
        };
        let device_type = NmDeviceType::from_u32(dict_u32(&dev, "DeviceType").unwrap_or(0));
        let ip4_address = dict_object_path(&dev, "Ip4Config")
            .and_then(|ip_path| get_all_blocking(conn, NM_DEST, &ip_path, NM_IP4_IFACE).ok())
            .and_then(|ip_props| extract_first_ipv4(ip_props.get("AddressData")));

        let wifi_metrics = if device_type == NmDeviceType::Wifi {
            if let Ok(wifi) = get_all_blocking(conn, NM_DEST, dev_path, NM_WIRELESS_IFACE) {
                let ap_info = dict_object_path(&wifi, "ActiveAccessPoint")
                    .and_then(|ap| get_all_blocking(conn, NM_DEST, &ap, NM_AP_IFACE).ok());
                parse_wifi_metrics(&wifi, ap_info.as_ref())
            } else {
                (None, None, None)
            }
        } else {
            (None, None, None)
        };

        devices.push(parse_device_info(
            dev_path.clone(),
            &dev,
            ip4_address,
            wifi_metrics,
        ));
    }

    let saved_connections = list_saved_connections_blocking(conn).unwrap_or_default();
    let access_points =
        collect_wifi_access_points_blocking(conn, &device_paths, &saved_connections);

    Ok(build_nm_snapshot(
        &root,
        primary_id,
        active_connections,
        devices,
        access_points,
        saved_connections,
    ))
}

#[derive(Default)]
struct NmWorkerCache {
    saved_connections: Vec<NmSavedConnection>,
    access_points: Vec<NmAccessPoint>,
    last_settings_fetch: Option<Instant>,
    last_ap_fetch: Option<Instant>,
}

async fn read_nm_snapshot_cached_async(
    conn: &zbus::Connection,
    cache: &mut NmWorkerCache,
    refresh_settings: bool,
    refresh_aps: bool,
) -> Option<NetworkManagerSnapshot> {
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
        let conn_info = parse_active_connection(path.clone(), &active_conn);
        if primary_path.as_deref() == Some(path.as_str()) && !conn_info.id.is_empty() {
            primary_id = Some(conn_info.id.clone());
        }
        active_connections.push(conn_info);
    }

    let device_paths: Vec<String> = dict_str_vec(&root, "Devices")
        .into_iter()
        .take(64)
        .collect();
    let mut devices = Vec::new();
    for dev_path in &device_paths {
        let Some(dev) = get_all_async(conn, NM_DEST, dev_path, NM_DEVICE_IFACE).await else {
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
            if let Some(wifi) = get_all_async(conn, NM_DEST, dev_path, NM_WIRELESS_IFACE).await {
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

        devices.push(parse_device_info(
            dev_path.clone(),
            &dev,
            ip4_address,
            wifi_metrics,
        ));
    }

    let now = Instant::now();
    if refresh_settings || cache.last_settings_fetch.is_none() {
        cache.saved_connections = list_saved_connections_async(conn).await;
        cache.last_settings_fetch = Some(now);
    }

    let wireless_enabled = dict_bool(&root, "WirelessEnabled").unwrap_or(false);
    if !wireless_enabled {
        cache.access_points.clear();
    } else if refresh_aps || cache.last_ap_fetch.is_none() {
        cache.access_points =
            collect_wifi_access_points_async(conn, &device_paths, &cache.saved_connections).await;
        cache.last_ap_fetch = Some(now);
    }

    Some(build_nm_snapshot(
        &root,
        primary_id,
        active_connections,
        devices,
        cache.access_points.clone(),
        cache.saved_connections.clone(),
    ))
}

pub(crate) fn parse_ssid_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn is_ap_secured(flags: u32, wpa_flags: u32, rsn_flags: u32) -> bool {
    (flags & 0x1) != 0 || wpa_flags != 0 || rsn_flags != 0
}

pub(crate) fn classify_ap_security(flags: u32, wpa_flags: u32, rsn_flags: u32) -> NmWifiSecurity {
    // NM_802_11_AP_SEC_KEY_MGMT_OWE = 0x800, NM_802_11_AP_SEC_KEY_MGMT_SAE = 0x400,
    // NM_802_11_AP_SEC_KEY_MGMT_802_1X = 0x200, NM_802_11_AP_SEC_KEY_MGMT_PSK = 0x100
    let combined = wpa_flags | rsn_flags;
    if (combined & 0x800) != 0 {
        NmWifiSecurity::Owe
    } else if (combined & 0x400) != 0 {
        NmWifiSecurity::Wpa3Sae
    } else if (combined & 0x200) != 0 {
        NmWifiSecurity::WpaEap
    } else if (combined & 0x100) != 0 || combined != 0 {
        NmWifiSecurity::WpaPsk
    } else if (flags & 0x1) != 0 {
        NmWifiSecurity::Wep
    } else {
        NmWifiSecurity::Open
    }
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
                    let mut cache = NmWorkerCache::default();
                    let mut last_snap =
                        read_nm_snapshot_cached_async(&conn, &mut cache, true, true).await;
                    if let Some(ref snap) = last_snap {
                        let _ = sender.send(snap.clone());
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
                        let Some(Some(Ok(msg))) = next_msg else {
                            break;
                        };
                        let (mut refresh_settings, mut refresh_aps) =
                            classify_nm_signal_refresh(&msg, &cache);

                        // Drain any queued signals that arrived in the same burst so we only query
                        // NetworkManager once per burst instead of triggering a D-Bus call storm.
                        while let Some(Some(Ok(extra))) =
                            futures_lite::future::poll_once(stream.next()).await
                        {
                            let (rs, ra) = classify_nm_signal_refresh(&extra, &cache);
                            refresh_settings |= rs;
                            refresh_aps |= ra;
                        }

                        if let Some(snap) = read_nm_snapshot_cached_async(
                            &conn,
                            &mut cache,
                            refresh_settings,
                            refresh_aps,
                        )
                        .await
                            && last_snap.as_ref() != Some(&snap)
                        {
                            last_snap = Some(snap.clone());
                            let _ = sender.send(snap);
                        }
                    }
                });
            })
            .ok();
        Self { stop, join }
    }
}

fn classify_nm_signal_refresh(msg: &zbus::Message, cache: &NmWorkerCache) -> (bool, bool) {
    let header = msg.header();
    let path = header.path().map(|p| p.as_str()).unwrap_or("");
    let member = header.member().map(|m| m.as_str()).unwrap_or("");
    let iface = header.interface().map(|i| i.as_str()).unwrap_or("");

    let refresh_settings = path.starts_with(NM_SETTINGS_PATH)
        || iface == NM_SETTINGS_IFACE
        || iface == NM_SETTINGS_CONN_IFACE
        || matches!(member, "NewConnection" | "ConnectionRemoved" | "Updated");

    let ap_cooldown_elapsed = cache
        .last_ap_fetch
        .is_none_or(|t| t.elapsed() >= NM_WIFI_REFRESH_COOLDOWN);

    let refresh_aps = refresh_settings
        || matches!(member, "AccessPointAdded" | "AccessPointRemoved")
        || ((path.contains("/AccessPoint/") || iface == NM_WIRELESS_IFACE) && ap_cooldown_elapsed);

    (refresh_settings, refresh_aps)
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
        assert_eq!(classify_ap_security(0, 0, 0), NmWifiSecurity::Open);
        assert_eq!(classify_ap_security(1, 0, 0), NmWifiSecurity::Wep);
        assert_eq!(classify_ap_security(1, 0x100, 0), NmWifiSecurity::WpaPsk);
        assert_eq!(classify_ap_security(1, 0, 0x400), NmWifiSecurity::Wpa3Sae);
        assert_eq!(classify_ap_security(0, 0, 0x800), NmWifiSecurity::Owe);
    }

    #[test]
    fn builds_wifi_connection_settings_for_wpa2_wpa3_sae_owe_and_open() {
        let wpa = build_wifi_connection_settings(
            "ITERA_Staff",
            NmWifiSecurity::WpaPsk,
            Some("rahasia123"),
            false,
        );
        assert!(wpa.contains_key("connection"));
        assert!(wpa.contains_key("802-11-wireless"));
        assert_eq!(
            wpa["802-11-wireless-security"].get("key-mgmt"),
            Some(&Value::from("wpa-psk"))
        );

        let sae = build_wifi_connection_settings(
            "ITERA_WiFi6",
            NmWifiSecurity::Wpa3Sae,
            Some("rahasia_sae"),
            false,
        );
        assert_eq!(
            sae["802-11-wireless-security"].get("key-mgmt"),
            Some(&Value::from("sae"))
        );

        let owe =
            build_wifi_connection_settings("ITERA_Public_OWE", NmWifiSecurity::Owe, None, false);
        assert_eq!(
            owe["802-11-wireless-security"].get("key-mgmt"),
            Some(&Value::from("owe"))
        );

        let open = build_wifi_connection_settings("ITERA_Guest", NmWifiSecurity::Open, None, true);
        assert!(!open.contains_key("802-11-wireless-security"));

        let mut raw_settings: HashMap<String, HashMap<String, OwnedValue>> = HashMap::new();
        let mut conn_sec = HashMap::new();
        conn_sec.insert(
            "id".into(),
            OwnedValue::try_from(Value::from("ITERA_Staff")).unwrap(),
        );
        conn_sec.insert(
            "uuid".into(),
            OwnedValue::try_from(Value::from("1234-5678")).unwrap(),
        );
        conn_sec.insert(
            "type".into(),
            OwnedValue::try_from(Value::from("802-11-wireless")).unwrap(),
        );
        conn_sec.insert(
            "timestamp".into(),
            OwnedValue::try_from(Value::from(1_700_000_000u64)).unwrap(),
        );
        let mut wifi_sec = HashMap::new();
        wifi_sec.insert(
            "ssid".into(),
            OwnedValue::try_from(Value::from(b"ITERA_Staff".to_vec())).unwrap(),
        );
        raw_settings.insert("connection".into(), conn_sec);
        raw_settings.insert("802-11-wireless".into(), wifi_sec);

        let saved = parse_saved_connection(
            "/org/freedesktop/NetworkManager/Settings/3".into(),
            &raw_settings,
        )
        .expect("saved profile parsed");
        assert_eq!(saved.id, "ITERA_Staff");
        assert_eq!(saved.ssid.as_deref(), Some("ITERA_Staff"));
        assert_eq!(saved.timestamp, 1_700_000_000);
    }
}
