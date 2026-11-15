//! UPower battery, peripheral, AC line, and lid-switch inspection (`org.freedesktop.UPower`).

use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{DbusClient, DbusSharedBus};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{
    dict_bool, dict_f64, dict_i64, dict_str, dict_u32, get_all_async, get_all_blocking,
};

const UPOWER_DEST: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const UPOWER_IFACE: &str = "org.freedesktop.UPower";
const UPOWER_DEVICE_IFACE: &str = "org.freedesktop.UPower.Device";
const UPOWER_DISPLAY_DEVICE_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

/// UPower device type (`UpDeviceKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UPowerDeviceKind {
    #[default]
    Unknown,
    LinePower,
    Battery,
    Ups,
    Monitor,
    Mouse,
    Keyboard,
    Pda,
    Phone,
    MediaPlayer,
    Tablet,
    Computer,
    GamingInput,
    Pen,
    Touchpad,
    Modem,
    Network,
    Headset,
    Speakers,
    Headphones,
    Video,
    OtherAudio,
    RemoteControl,
    Printer,
    Scanner,
    Camera,
    Wearable,
    Toy,
    BluetoothGeneric,
    Other(u32),
}

impl UPowerDeviceKind {
    /// Decodes the UPower `Type` numeric constant.
    #[must_use]
    pub fn from_u32(val: u32) -> Self {
        match val {
            0 => Self::Unknown,
            1 => Self::LinePower,
            2 => Self::Battery,
            3 => Self::Ups,
            4 => Self::Monitor,
            5 => Self::Mouse,
            6 => Self::Keyboard,
            7 => Self::Pda,
            8 => Self::Phone,
            9 => Self::MediaPlayer,
            10 => Self::Tablet,
            11 => Self::Computer,
            12 => Self::GamingInput,
            13 => Self::Pen,
            14 => Self::Touchpad,
            15 => Self::Modem,
            16 => Self::Network,
            17 => Self::Headset,
            18 => Self::Speakers,
            19 => Self::Headphones,
            20 => Self::Video,
            21 => Self::OtherAudio,
            22 => Self::RemoteControl,
            23 => Self::Printer,
            24 => Self::Scanner,
            25 => Self::Camera,
            26 => Self::Wearable,
            27 => Self::Toy,
            28 => Self::BluetoothGeneric,
            other => Self::Other(other),
        }
    }
}

/// UPower charging/discharging state (`UpDeviceState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UPowerDeviceState {
    #[default]
    Unknown,
    Charging,
    Discharging,
    Empty,
    FullyCharged,
    PendingCharge,
    PendingDischarge,
}

impl UPowerDeviceState {
    /// Decodes the UPower `State` numeric constant.
    #[must_use]
    pub fn from_u32(val: u32) -> Self {
        match val {
            1 => Self::Charging,
            2 => Self::Discharging,
            3 => Self::Empty,
            4 => Self::FullyCharged,
            5 => Self::PendingCharge,
            6 => Self::PendingDischarge,
            _ => Self::Unknown,
        }
    }
}

/// Battery or power-source device reported by `org.freedesktop.UPower.Device`.
#[derive(Debug, Clone, PartialEq)]
pub struct UPowerDevice {
    pub object_path: String,
    pub native_path: String,
    pub vendor: String,
    pub model: String,
    pub serial: String,
    pub kind: UPowerDeviceKind,
    pub state: UPowerDeviceState,
    pub percentage: f64,
    pub is_present: bool,
    pub is_rechargeable: bool,
    pub power_supply: bool,
    pub is_display_device: bool,
    pub energy_rate_w: f64,
    pub time_to_empty_secs: i64,
    pub time_to_full_secs: i64,
    pub temperature_c: Option<f64>,
    pub icon_name: Option<String>,
}

/// Aggregate snapshot of UPower AC/lid flags and battery/peripheral devices.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UPowerSnapshot {
    pub daemon_version: String,
    pub on_battery: bool,
    pub lid_is_closed: bool,
    pub lid_is_present: bool,
    pub devices: Vec<UPowerDevice>,
}

impl UPowerSnapshot {
    /// Returns the composite system `DisplayDevice` battery when exposed by UPower.
    #[must_use]
    pub fn display_device(&self) -> Option<&UPowerDevice> {
        self.devices.iter().find(|d| d.is_display_device)
    }

    /// Returns wireless/USB peripheral devices (mice, keyboards, headsets, phones, etc.).
    pub fn peripherals(&self) -> impl Iterator<Item = &UPowerDevice> {
        self.devices.iter().filter(|d| {
            !d.is_display_device && !d.power_supply && d.kind != UPowerDeviceKind::LinePower
        })
    }
}

impl Dbus {
    /// Subscribes to UPower battery, peripheral, AC, and lid state changes.
    pub fn upower<M: 'static>(map: impl Fn(UPowerSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusUPower {
                key: "dbus:upower".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Reads the current [`UPowerSnapshot`] from the system bus.
    pub fn upower_snapshot(&self) -> Result<UPowerSnapshot> {
        let conn = self.connection(DbusBus::System)?;
        read_upower_snapshot_blocking(&conn)
    }
}

pub(crate) fn spawn_upower_worker(
    sender: calloop::channel::Sender<UPowerSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-upower",
        Some(UPOWER_DEST),
        None,
        sender,
        shared,
        |conn| async move { read_upower_snapshot_async(&conn).await },
    )
}

fn read_upower_snapshot_blocking(conn: &zbus::blocking::Connection) -> Result<UPowerSnapshot> {
    let root = get_all_blocking(conn, UPOWER_DEST, UPOWER_PATH, UPOWER_IFACE)?;
    let reply = conn
        .call_method(
            Some(UPOWER_DEST),
            UPOWER_PATH,
            Some(UPOWER_IFACE),
            "EnumerateDevices",
            &(),
        )
        .map_err(|e| WaywiError::DbusError(format!("UPower.EnumerateDevices failed: {e}")))?;
    let mut paths: Vec<OwnedObjectPath> = reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;

    let mut devices = Vec::with_capacity(paths.len() + 1);
    if let Ok(display) = get_all_blocking(
        conn,
        UPOWER_DEST,
        UPOWER_DISPLAY_DEVICE_PATH,
        UPOWER_DEVICE_IFACE,
    ) {
        devices.push(parse_upower_device(
            UPOWER_DISPLAY_DEVICE_PATH,
            &display,
            true,
        ));
    }
    for path in paths.drain(..).take(64) {
        let p = path.as_str();
        if p == UPOWER_DISPLAY_DEVICE_PATH {
            continue;
        }
        if let Ok(props) = get_all_blocking(conn, UPOWER_DEST, p, UPOWER_DEVICE_IFACE) {
            devices.push(parse_upower_device(p, &props, false));
        }
    }
    Ok(assemble_upower_snapshot(&root, devices))
}

async fn read_upower_snapshot_async(conn: &zbus::Connection) -> Option<UPowerSnapshot> {
    let root = get_all_async(conn, UPOWER_DEST, UPOWER_PATH, UPOWER_IFACE).await?;
    let reply = conn
        .call_method(
            Some(UPOWER_DEST),
            UPOWER_PATH,
            Some(UPOWER_IFACE),
            "EnumerateDevices",
            &(),
        )
        .await
        .ok()?;
    let paths: Vec<OwnedObjectPath> = reply.body().deserialize().ok()?;

    let mut devices = Vec::with_capacity(paths.len() + 1);
    if let Some(display) = get_all_async(
        conn,
        UPOWER_DEST,
        UPOWER_DISPLAY_DEVICE_PATH,
        UPOWER_DEVICE_IFACE,
    )
    .await
    {
        devices.push(parse_upower_device(
            UPOWER_DISPLAY_DEVICE_PATH,
            &display,
            true,
        ));
    }
    for path in paths.into_iter().take(64) {
        let p = path.as_str();
        if p == UPOWER_DISPLAY_DEVICE_PATH {
            continue;
        }
        if let Some(props) = get_all_async(conn, UPOWER_DEST, p, UPOWER_DEVICE_IFACE).await {
            devices.push(parse_upower_device(p, &props, false));
        }
    }
    Some(assemble_upower_snapshot(&root, devices))
}

fn assemble_upower_snapshot(
    root: &HashMap<String, OwnedValue>,
    devices: Vec<UPowerDevice>,
) -> UPowerSnapshot {
    UPowerSnapshot {
        daemon_version: dict_str(root, "DaemonVersion").unwrap_or_default(),
        on_battery: dict_bool(root, "OnBattery").unwrap_or(false),
        lid_is_closed: dict_bool(root, "LidIsClosed").unwrap_or(false),
        lid_is_present: dict_bool(root, "LidIsPresent").unwrap_or(false),
        devices,
    }
}

pub(crate) fn parse_upower_device(
    object_path: &str,
    props: &HashMap<String, OwnedValue>,
    is_display_device: bool,
) -> UPowerDevice {
    UPowerDevice {
        object_path: object_path.to_owned(),
        native_path: dict_str(props, "NativePath").unwrap_or_default(),
        vendor: dict_str(props, "Vendor").unwrap_or_default(),
        model: dict_str(props, "Model").unwrap_or_default(),
        serial: dict_str(props, "Serial").unwrap_or_default(),
        kind: UPowerDeviceKind::from_u32(dict_u32(props, "Type").unwrap_or(0)),
        state: UPowerDeviceState::from_u32(dict_u32(props, "State").unwrap_or(0)),
        percentage: dict_f64(props, "Percentage").unwrap_or(0.0),
        is_present: dict_bool(props, "IsPresent").unwrap_or(false),
        is_rechargeable: dict_bool(props, "IsRechargeable").unwrap_or(false),
        power_supply: dict_bool(props, "PowerSupply").unwrap_or(false),
        is_display_device,
        energy_rate_w: dict_f64(props, "EnergyRate").unwrap_or(0.0),
        time_to_empty_secs: dict_i64(props, "TimeToEmpty").unwrap_or(0),
        time_to_full_secs: dict_i64(props, "TimeToFull").unwrap_or(0),
        temperature_c: dict_f64(props, "Temperature").filter(|t| *t > -273.0),
        icon_name: dict_str(props, "IconName").filter(|s| !s.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn parses_peripheral_upower_device_properties() {
        let mut props: HashMap<String, OwnedValue> = HashMap::new();
        props.insert(
            "Model".into(),
            OwnedValue::try_from(Value::from("MX Master 3S")).unwrap(),
        );
        props.insert(
            "Vendor".into(),
            OwnedValue::try_from(Value::from("Logitech")).unwrap(),
        );
        props.insert(
            "Type".into(),
            OwnedValue::try_from(Value::from(5u32)).unwrap(),
        );
        props.insert(
            "State".into(),
            OwnedValue::try_from(Value::from(2u32)).unwrap(),
        );
        props.insert(
            "Percentage".into(),
            OwnedValue::try_from(Value::from(82.0f64)).unwrap(),
        );
        props.insert(
            "IsPresent".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        props.insert(
            "IsRechargeable".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        props.insert(
            "PowerSupply".into(),
            OwnedValue::try_from(Value::from(false)).unwrap(),
        );

        let dev = parse_upower_device("/org/freedesktop/UPower/devices/mouse_hidpp", &props, false);
        assert_eq!(dev.kind, UPowerDeviceKind::Mouse);
        assert_eq!(dev.state, UPowerDeviceState::Discharging);
        assert!((dev.percentage - 82.0).abs() < 0.01);
        assert!(!dev.power_supply);

        let snap = UPowerSnapshot {
            on_battery: false,
            devices: vec![dev],
            ..Default::default()
        };
        assert_eq!(snap.peripherals().count(), 1);
    }
}
