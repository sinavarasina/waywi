//! Shared `zbus` property-map and `ObjectManager` deserialization helpers.

use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::error::{Result, WaywiError};

pub(crate) type ManagedObjects =
    HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

pub(crate) fn get_all_blocking(
    conn: &zbus::blocking::Connection,
    dest: &str,
    path: &str,
    iface: &str,
) -> Result<HashMap<String, OwnedValue>> {
    let reply = conn
        .call_method(
            Some(dest),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &(iface,),
        )
        .map_err(|e| WaywiError::DbusError(format!("Properties.GetAll({iface}) failed: {e}")))?;
    reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))
}

pub(crate) async fn get_all_async(
    conn: &zbus::Connection,
    dest: &str,
    path: &str,
    iface: &str,
) -> Option<HashMap<String, OwnedValue>> {
    let reply = conn
        .call_method(
            Some(dest),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &(iface,),
        )
        .await
        .ok()?;
    reply.body().deserialize().ok()
}

pub(crate) fn get_managed_objects_blocking(
    conn: &zbus::blocking::Connection,
    dest: &str,
    path: &str,
) -> Result<ManagedObjects> {
    let reply = conn
        .call_method(
            Some(dest),
            path,
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .map_err(|e| WaywiError::DbusError(format!("GetManagedObjects({dest}) failed: {e}")))?;
    reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))
}

pub(crate) async fn get_managed_objects_async(
    conn: &zbus::Connection,
    dest: &str,
    path: &str,
) -> Option<ManagedObjects> {
    let reply = conn
        .call_method(
            Some(dest),
            path,
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await
        .ok()?;
    reply.body().deserialize().ok()
}

pub(crate) fn unwrap_value<'a>(mut val: &'a Value<'a>) -> &'a Value<'a> {
    while let Value::Value(inner) = val {
        val = inner.as_ref();
    }
    val
}

pub(crate) fn dict_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match props.get(key).map(|v| &**v) {
        Some(Value::Str(s)) => Some(s.to_string()),
        Some(Value::ObjectPath(p)) => Some(p.to_string()),
        _ => None,
    }
}

pub(crate) fn dict_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match props.get(key).map(|v| &**v) {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}

pub(crate) fn dict_u8(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    match props.get(key).map(|v| &**v) {
        Some(Value::U8(v)) => Some(*v),
        Some(Value::U16(v)) => u8::try_from(*v).ok(),
        Some(Value::U32(v)) => u8::try_from(*v).ok(),
        _ => None,
    }
}

pub(crate) fn dict_i16(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i16> {
    match props.get(key).map(|v| &**v) {
        Some(Value::I16(v)) => Some(*v),
        Some(Value::I32(v)) => i16::try_from(*v).ok(),
        _ => None,
    }
}

pub(crate) fn dict_i32(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    match props.get(key).map(|v| &**v) {
        Some(Value::I32(v)) => Some(*v),
        Some(Value::I16(v)) => Some(i32::from(*v)),
        _ => None,
    }
}

pub(crate) fn dict_u32(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    match props.get(key).map(|v| &**v) {
        Some(Value::U32(v)) => Some(*v),
        Some(Value::U16(v)) => Some(u32::from(*v)),
        Some(Value::U8(v)) => Some(u32::from(*v)),
        Some(Value::I32(v)) if *v >= 0 => Some(*v as u32),
        _ => None,
    }
}

pub(crate) fn dict_i64(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    match props.get(key).map(|v| &**v) {
        Some(Value::I64(v)) => Some(*v),
        Some(Value::I32(v)) => Some(i64::from(*v)),
        Some(Value::U64(v)) => i64::try_from(*v).ok(),
        _ => None,
    }
}

pub(crate) fn dict_u64(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u64> {
    match props.get(key).map(|v| &**v) {
        Some(Value::U64(v)) => Some(*v),
        Some(Value::U32(v)) => Some(u64::from(*v)),
        Some(Value::I64(v)) if *v >= 0 => Some(*v as u64),
        _ => None,
    }
}

pub(crate) fn dict_f64(props: &HashMap<String, OwnedValue>, key: &str) -> Option<f64> {
    match props.get(key).map(|v| &**v) {
        Some(Value::F64(v)) => Some(*v),
        Some(Value::I64(v)) => Some(*v as f64),
        Some(Value::U64(v)) => Some(*v as f64),
        _ => None,
    }
}

pub(crate) fn dict_bytes(props: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<u8>> {
    props
        .get(key)
        .and_then(|v| Vec::<u8>::try_from(v.clone()).ok())
}

pub(crate) fn dict_object_path(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let path = match props.get(key).map(|v| &**v) {
        Some(Value::ObjectPath(p)) => Some(p.to_string()),
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }?;
    (!path.is_empty() && path != "/").then_some(path)
}

pub(crate) fn dict_str_vec(props: &HashMap<String, OwnedValue>, key: &str) -> Vec<String> {
    let Some(Value::Array(arr)) = props.get(key).map(|v| &**v) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| match item {
            Value::Str(s) => Some(s.to_string()),
            Value::ObjectPath(p) => Some(p.to_string()),
            _ => None,
        })
        .collect()
}
