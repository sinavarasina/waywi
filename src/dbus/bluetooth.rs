//! BlueZ Bluetooth adapter, device, battery, and connection management (`org.bluez`).

use zbus::zvariant::{OwnedObjectPath, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{DbusClient, DbusSharedBus};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{
    ManagedObjects, dict_bool, dict_i16, dict_str, dict_str_vec, dict_u8,
    get_managed_objects_async, get_managed_objects_blocking,
};

const BLUEZ_DEST: &str = "org.bluez";
const BLUEZ_ROOT_PATH: &str = "/";
const BLUEZ_ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const BLUEZ_DEVICE_IFACE: &str = "org.bluez.Device1";
const BLUEZ_BATTERY_IFACE: &str = "org.bluez.Battery1";

/// Bluetooth controller state (`org.bluez.Adapter1`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BluetoothAdapter {
    pub object_path: String,
    pub address: String,
    pub name: String,
    pub alias: String,
    pub powered: bool,
    pub discoverable: bool,
    pub pairable: bool,
    pub discovering: bool,
}

/// Discovered or paired Bluetooth device (`org.bluez.Device1` + optional `org.bluez.Battery1`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BluetoothDevice {
    pub object_path: String,
    pub adapter_path: String,
    pub address: String,
    pub name: String,
    pub alias: String,
    pub icon: Option<String>,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub blocked: bool,
    pub rssi: Option<i16>,
    pub battery_percentage: Option<u8>,
    pub uuids: Vec<String>,
}

/// Aggregate snapshot of BlueZ adapters and devices.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BluetoothSnapshot {
    pub adapters: Vec<BluetoothAdapter>,
    pub devices: Vec<BluetoothDevice>,
}

impl Dbus {
    /// Subscribes to BlueZ Bluetooth adapter, device, and battery updates.
    pub fn bluetooth<M: 'static>(
        map: impl Fn(BluetoothSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusBluetooth {
                key: "dbus:bluetooth".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Reads the current [`BluetoothSnapshot`] from `org.bluez`.
    pub fn bluetooth_snapshot(&self) -> Result<BluetoothSnapshot> {
        let conn = self.connection(DbusBus::System)?;
        let objs = get_managed_objects_blocking(&conn, BLUEZ_DEST, BLUEZ_ROOT_PATH)?;
        Ok(parse_bluez_objects(&objs))
    }

    /// Powers a Bluetooth adapter (`org.bluez.Adapter1.Powered`) on or off.
    pub fn set_bluetooth_powered(&self, adapter_path: &str, powered: bool) -> Result<()> {
        self.set_property(
            DbusBus::System,
            BLUEZ_DEST,
            adapter_path,
            BLUEZ_ADAPTER_IFACE,
            "Powered",
            Value::from(powered),
        )
    }

    /// Starts Bluetooth device discovery on `adapter_path`.
    pub fn start_bluetooth_discovery(&self, adapter_path: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::System,
            BLUEZ_DEST,
            adapter_path,
            BLUEZ_ADAPTER_IFACE,
            "StartDiscovery",
        )
    }

    /// Stops Bluetooth device discovery on `adapter_path`.
    pub fn stop_bluetooth_discovery(&self, adapter_path: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::System,
            BLUEZ_DEST,
            adapter_path,
            BLUEZ_ADAPTER_IFACE,
            "StopDiscovery",
        )
    }

    /// Connects a known Bluetooth device (`org.bluez.Device1.Connect`).
    pub fn connect_bluetooth_device(&self, device_path: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::System,
            BLUEZ_DEST,
            device_path,
            BLUEZ_DEVICE_IFACE,
            "Connect",
        )
    }

    /// Disconnects a connected Bluetooth device (`org.bluez.Device1.Disconnect`).
    pub fn disconnect_bluetooth_device(&self, device_path: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::System,
            BLUEZ_DEST,
            device_path,
            BLUEZ_DEVICE_IFACE,
            "Disconnect",
        )
    }

    /// Pairs a discovered Bluetooth device (`org.bluez.Device1.Pair`).
    pub fn pair_bluetooth_device(&self, device_path: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::System,
            BLUEZ_DEST,
            device_path,
            BLUEZ_DEVICE_IFACE,
            "Pair",
        )
    }

    /// Sets the `Trusted` flag on a Bluetooth device (`org.bluez.Device1.Trusted`).
    pub fn trust_bluetooth_device(&self, device_path: &str, trusted: bool) -> Result<()> {
        self.set_property(
            DbusBus::System,
            BLUEZ_DEST,
            device_path,
            BLUEZ_DEVICE_IFACE,
            "Trusted",
            Value::from(trusted),
        )
    }

    /// Unpairs/removes a Bluetooth device from its adapter (`org.bluez.Adapter1.RemoveDevice`).
    pub fn remove_bluetooth_device(&self, adapter_path: &str, device_path: &str) -> Result<()> {
        let conn = self.connection(DbusBus::System)?;
        let obj = OwnedObjectPath::try_from(device_path.to_owned())
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        conn.call_method(
            Some(BLUEZ_DEST),
            adapter_path,
            Some(BLUEZ_ADAPTER_IFACE),
            "RemoveDevice",
            &(obj,),
        )
        .map_err(|e| WaywiError::DbusError(format!("Adapter1.RemoveDevice failed: {e}")))?;
        Ok(())
    }
}

pub(crate) fn spawn_bluetooth_worker(
    sender: calloop::channel::Sender<BluetoothSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-bluez",
        Some(BLUEZ_DEST),
        None,
        sender,
        shared,
        |conn| async move {
            let objs = get_managed_objects_async(&conn, BLUEZ_DEST, BLUEZ_ROOT_PATH).await?;
            Some(parse_bluez_objects(&objs))
        },
    )
}

pub(crate) fn parse_bluez_objects(objects: &ManagedObjects) -> BluetoothSnapshot {
    let mut adapters = Vec::new();
    let mut devices = Vec::new();

    for (path, ifaces) in objects {
        let path_str = path.to_string();
        if let Some(props) = ifaces.get(BLUEZ_ADAPTER_IFACE) {
            adapters.push(BluetoothAdapter {
                object_path: path_str.clone(),
                address: dict_str(props, "Address").unwrap_or_default(),
                name: dict_str(props, "Name").unwrap_or_default(),
                alias: dict_str(props, "Alias").unwrap_or_default(),
                powered: dict_bool(props, "Powered").unwrap_or(false),
                discoverable: dict_bool(props, "Discoverable").unwrap_or(false),
                pairable: dict_bool(props, "Pairable").unwrap_or(false),
                discovering: dict_bool(props, "Discovering").unwrap_or(false),
            });
        }
        if let Some(props) = ifaces.get(BLUEZ_DEVICE_IFACE) {
            let battery_percentage = ifaces
                .get(BLUEZ_BATTERY_IFACE)
                .and_then(|bat| dict_u8(bat, "Percentage"));
            devices.push(BluetoothDevice {
                object_path: path_str,
                adapter_path: dict_str(props, "Adapter").unwrap_or_default(),
                address: dict_str(props, "Address").unwrap_or_default(),
                name: dict_str(props, "Name").unwrap_or_default(),
                alias: dict_str(props, "Alias").unwrap_or_default(),
                icon: dict_str(props, "Icon").filter(|s| !s.is_empty()),
                paired: dict_bool(props, "Paired").unwrap_or(false),
                trusted: dict_bool(props, "Trusted").unwrap_or(false),
                connected: dict_bool(props, "Connected").unwrap_or(false),
                blocked: dict_bool(props, "Blocked").unwrap_or(false),
                rssi: dict_i16(props, "RSSI"),
                battery_percentage,
                uuids: dict_str_vec(props, "UUIDs"),
            });
        }
    }

    adapters.sort_by(|a, b| a.object_path.cmp(&b.object_path));
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.paired.cmp(&a.paired))
            .then_with(|| a.alias.cmp(&b.alias))
    });
    BluetoothSnapshot { adapters, devices }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedValue, Value};

    use super::*;

    #[test]
    fn parses_bluez_managed_objects_with_battery() {
        let mut objs: ManagedObjects = HashMap::new();
        let mut hci_ifaces = HashMap::new();
        let mut hci_props = HashMap::new();
        hci_props.insert(
            "Powered".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        hci_props.insert(
            "Alias".into(),
            OwnedValue::try_from(Value::from("hci0")).unwrap(),
        );
        hci_ifaces.insert(BLUEZ_ADAPTER_IFACE.to_owned(), hci_props);
        objs.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0").unwrap(),
            hci_ifaces,
        );

        let mut dev_ifaces = HashMap::new();
        let mut dev_props = HashMap::new();
        dev_props.insert(
            "Address".into(),
            OwnedValue::try_from(Value::from("AA:BB:CC:DD:EE:FF")).unwrap(),
        );
        dev_props.insert(
            "Alias".into(),
            OwnedValue::try_from(Value::from("WH-1000XM5")).unwrap(),
        );
        dev_props.insert(
            "Connected".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        dev_props.insert(
            "Paired".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        let mut bat_props = HashMap::new();
        bat_props.insert(
            "Percentage".into(),
            OwnedValue::try_from(Value::from(75u8)).unwrap(),
        );
        dev_ifaces.insert(BLUEZ_DEVICE_IFACE.to_owned(), dev_props);
        dev_ifaces.insert(BLUEZ_BATTERY_IFACE.to_owned(), bat_props);
        objs.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").unwrap(),
            dev_ifaces,
        );

        let snap = parse_bluez_objects(&objs);
        assert_eq!(snap.adapters.len(), 1);
        assert!(snap.adapters[0].powered);
        assert_eq!(snap.devices.len(), 1);
        assert_eq!(snap.devices[0].alias, "WH-1000XM5");
        assert_eq!(snap.devices[0].battery_percentage, Some(75));
    }
}
