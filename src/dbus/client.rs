use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use zbus::zvariant::{OwnedValue, Value};

use crate::error::{Result, WaywiError};

use super::types::{DbusBus, DbusValue};

/// Shared async `zbus::Connection` slot reused across [`DbusClient`] and D-Bus observer workers.
pub(crate) type DbusSharedBus = Arc<Mutex<Option<zbus::Connection>>>;

pub(crate) fn connect_shared_bus(bus: DbusBus, shared: &DbusSharedBus) -> Result<zbus::Connection> {
    let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ref conn) = *guard {
        return Ok(conn.clone());
    }
    let conn = futures_lite::future::block_on(async move {
        match bus {
            DbusBus::Session => zbus::Connection::session().await,
            DbusBus::System => zbus::Connection::system().await,
        }
    })
    .map_err(|e| WaywiError::DbusError(format!("D-Bus connect failed: {e}")))?;
    *guard = Some(conn.clone());
    Ok(conn)
}

/// D-Bus client backed by `zbus` (blocking connection + dedicated async-io workers).
pub struct DbusClient {
    session: RwLock<Option<zbus::blocking::Connection>>,
    system: RwLock<Option<zbus::blocking::Connection>>,
    shared_session: DbusSharedBus,
    shared_system: DbusSharedBus,
    pub(crate) notifications_conn: Arc<Mutex<Option<zbus::Connection>>>,
    pub(crate) bluetooth_agent_pending: super::bluetooth::PendingAgentMap,
}

impl Default for DbusClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DbusClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: RwLock::new(None),
            system: RwLock::new(None),
            shared_session: Arc::new(Mutex::new(None)),
            shared_system: Arc::new(Mutex::new(None)),
            notifications_conn: Arc::new(Mutex::new(None)),
            bluetooth_agent_pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn shared_bus(&self, bus: DbusBus) -> DbusSharedBus {
        match bus {
            DbusBus::Session => Arc::clone(&self.shared_session),
            DbusBus::System => Arc::clone(&self.shared_system),
        }
    }

    pub(super) fn connection(&self, bus: DbusBus) -> Result<zbus::blocking::Connection> {
        let slot = match bus {
            DbusBus::Session => &self.session,
            DbusBus::System => &self.system,
        };
        if let Ok(guard) = slot.read()
            && let Some(ref conn) = *guard
        {
            return Ok(conn.clone());
        }
        let shared = self.shared_bus(bus);
        let async_conn = connect_shared_bus(bus, &shared)?;
        let conn = zbus::blocking::Connection::from(async_conn);
        if let Ok(mut guard) = slot.write() {
            *guard = Some(conn.clone());
        }
        Ok(conn)
    }

    /// Reads a D-Bus property via `org.freedesktop.DBus.Properties.Get`.
    pub fn get_property(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        property: &str,
    ) -> Result<DbusValue> {
        let conn = self.connection(bus)?;
        let proxy = zbus::blocking::fdo::PropertiesProxy::builder(&conn)
            .destination(destination)
            .and_then(|b| b.path(path))
            .map_err(|e| WaywiError::DbusError(e.to_string()))?
            .build()
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        let iface_name = zbus::names::InterfaceName::try_from(interface)
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        let val = proxy
            .get(iface_name, property)
            .map_err(|e| WaywiError::DbusError(format!("property read failed: {e}")))?;
        Ok(owned_value_to_dbus(&val))
    }

    /// Calls a zero-argument or unit D-Bus method cleanly.
    pub fn call_method_unit(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
    ) -> Result<()> {
        let conn = self.connection(bus)?;
        conn.call_method(Some(destination), path, Some(interface), method, &())
            .map_err(|e| WaywiError::DbusError(format!("method `{method}` failed: {e}")))?;
        Ok(())
    }

    /// Writes a D-Bus property via `org.freedesktop.DBus.Properties.Set`.
    pub(super) fn set_property(
        &self,
        bus: DbusBus,
        destination: &str,
        path: &str,
        interface: &str,
        property: &str,
        value: Value<'_>,
    ) -> Result<()> {
        let conn = self.connection(bus)?;
        conn.call_method(
            Some(destination),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &(interface, property, value),
        )
        .map_err(|e| WaywiError::DbusError(format!("property `{property}` write failed: {e}")))?;
        Ok(())
    }
}

pub(super) fn owned_value_to_dbus(val: &OwnedValue) -> DbusValue {
    value_to_dbus(val)
}

pub(super) fn value_to_dbus(val: &Value<'_>) -> DbusValue {
    match val {
        Value::Str(s) => DbusValue::String(s.to_string()),
        Value::ObjectPath(p) => DbusValue::String(p.to_string()),
        Value::Bool(b) => DbusValue::Bool(*b),
        Value::I16(v) => DbusValue::I64(i64::from(*v)),
        Value::I32(v) => DbusValue::I64(i64::from(*v)),
        Value::I64(v) => DbusValue::I64(*v),
        Value::U8(v) => DbusValue::U64(u64::from(*v)),
        Value::U16(v) => DbusValue::U64(u64::from(*v)),
        Value::U32(v) => DbusValue::U64(u64::from(*v)),
        Value::U64(v) => DbusValue::U64(*v),
        Value::F64(v) => DbusValue::F64(*v),
        Value::Array(arr) => DbusValue::Array(arr.iter().map(value_to_dbus).collect()),
        Value::Structure(st) => DbusValue::Array(st.fields().iter().map(value_to_dbus).collect()),
        Value::Dict(dict) => {
            let mut map = HashMap::new();
            for (k, v) in dict.iter() {
                if let Value::Str(ks) = k {
                    map.insert(ks.to_string(), value_to_dbus(v));
                }
            }
            DbusValue::Dict(map)
        }
        Value::Value(inner) => value_to_dbus(inner),
        other => DbusValue::String(format!("{other:?}")),
    }
}
