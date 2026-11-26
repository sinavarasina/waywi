//! `iio-sensor-proxy` hardware sensor integration (`net.hadess.SensorProxy`).
//!
//! Streams accelerometer orientation (for touchscreen auto-rotate), ambient light level
//! (for auto-brightness), proximity, and compass heading zero-polling via D-Bus
//! `PropertiesChanged`, automatically claiming sensors while subscribed and releasing them
//! cleanly on drop.

use std::collections::HashMap;

use futures_lite::StreamExt;
use zbus::zvariant::OwnedValue;

use crate::arena::FrameSharedFn;
use crate::error::Result;
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{DbusClient, DbusSharedBus, connect_shared_bus};
use super::types::{Dbus, DbusBus};
use super::worker::DbusWorkerHandle;
use super::zvariant_helpers::{dict_bool, dict_f64, dict_str, get_all};

const SENSOR_DEST: &str = "net.hadess.SensorProxy";
const SENSOR_PATH: &str = "/net/hadess/SensorProxy";
const SENSOR_IFACE: &str = "net.hadess.SensorProxy";

/// Device orientation reported by `net.hadess.SensorProxy.AccelerometerOrientation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SensorOrientation {
    #[default]
    Undefined,
    Normal,
    BottomUp,
    LeftUp,
    RightUp,
}

impl SensorOrientation {
    #[must_use]
    pub fn from_proxy_str(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "normal" => Self::Normal,
            "bottom-up" => Self::BottomUp,
            "left-up" => Self::LeftUp,
            "right-up" => Self::RightUp,
            _ => Self::Undefined,
        }
    }
}

/// Unit reported by `net.hadess.SensorProxy.LightLevelUnit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LightLevelUnit {
    #[default]
    Unknown,
    Lux,
    Vendor,
}

impl LightLevelUnit {
    #[must_use]
    pub fn from_proxy_str(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "lux" => Self::Lux,
            "vendor" => Self::Vendor,
            _ => Self::Unknown,
        }
    }
}

/// Snapshot of `net.hadess.SensorProxy` readings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SensorProxySnapshot {
    pub has_accelerometer: bool,
    pub accelerometer_orientation: SensorOrientation,
    pub has_ambient_light: bool,
    pub light_level: f64,
    pub light_level_unit: LightLevelUnit,
    pub has_proximity: bool,
    pub proximity_near: bool,
}

impl Dbus {
    /// Subscribes to `net.hadess.SensorProxy` accelerometer, ambient light, and proximity
    /// changes on the system bus. Claims available sensors while active and releases them
    /// cleanly when unsubscribed.
    pub fn sensors<M: 'static>(
        map: impl Fn(SensorProxySnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusSensors {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for `net.hadess.SensorProxy` queries.
#[derive(Clone, Copy)]
pub struct SensorsClient<'a>(pub(super) &'a DbusClient);

impl SensorsClient<'_> {
    /// Returns the latest cached [`SensorProxySnapshot`] from an active [`Dbus::sensors`]
    /// subscription without issuing any D-Bus calls.
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<SensorProxySnapshot> {
        self.0.cache.sensors.read().ok().and_then(|g| *g)
    }

    /// Reads the current `net.hadess.SensorProxy` properties synchronously from the system bus.
    pub fn snapshot_blocking(&self) -> Result<SensorProxySnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let props = get_all(&conn, SENSOR_DEST, SENSOR_PATH, SENSOR_IFACE).await?;
            Ok(parse_sensor_props(&props))
        })
    }
}

pub(crate) fn parse_sensor_props(props: &HashMap<String, OwnedValue>) -> SensorProxySnapshot {
    SensorProxySnapshot {
        has_accelerometer: dict_bool(props, "HasAccelerometer").unwrap_or(false),
        accelerometer_orientation: dict_str(props, "AccelerometerOrientation")
            .as_deref()
            .map(SensorOrientation::from_proxy_str)
            .unwrap_or_default(),
        has_ambient_light: dict_bool(props, "HasAmbientLight").unwrap_or(false),
        light_level: dict_f64(props, "LightLevel")
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(0.0),
        light_level_unit: dict_str(props, "LightLevelUnit")
            .as_deref()
            .map(LightLevelUnit::from_proxy_str)
            .unwrap_or_default(),
        has_proximity: dict_bool(props, "HasProximity").unwrap_or(false),
        proximity_near: dict_bool(props, "ProximityNear").unwrap_or(false),
    }
}

pub(crate) struct SensorsWorker(#[allow(dead_code)] DbusWorkerHandle);

impl SensorsWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<SensorProxySnapshot>,
        shared: DbusSharedBus,
    ) -> Self {
        Self(DbusWorkerHandle::spawn("waywi-dbus-sensors", move |stop| {
            let Ok(conn) = connect_shared_bus(DbusBus::System, &shared) else {
                return;
            };
            futures_lite::future::block_on(async move {
                let Ok(initial_props) =
                    get_all(&conn, SENSOR_DEST, SENSOR_PATH, SENSOR_IFACE).await
                else {
                    return;
                };
                let initial = parse_sensor_props(&initial_props);

                let claimed_accel = if initial.has_accelerometer {
                    conn.call_method(
                        Some(SENSOR_DEST),
                        SENSOR_PATH,
                        Some(SENSOR_IFACE),
                        "ClaimAccelerometer",
                        &(),
                    )
                    .await
                    .is_ok()
                } else {
                    false
                };
                let claimed_light = if initial.has_ambient_light {
                    conn.call_method(
                        Some(SENSOR_DEST),
                        SENSOR_PATH,
                        Some(SENSOR_IFACE),
                        "ClaimLight",
                        &(),
                    )
                    .await
                    .is_ok()
                } else {
                    false
                };
                let claimed_prox = if initial.has_proximity {
                    conn.call_method(
                        Some(SENSOR_DEST),
                        SENSOR_PATH,
                        Some(SENSOR_IFACE),
                        "ClaimProximity",
                        &(),
                    )
                    .await
                    .is_ok()
                } else {
                    false
                };

                let mut last = get_all(&conn, SENSOR_DEST, SENSOR_PATH, SENSOR_IFACE)
                    .await
                    .map(|p| parse_sensor_props(&p))
                    .unwrap_or(initial);
                let _ = sender.send(last);

                let rule = match zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender(SENSOR_DEST)
                    .and_then(|b| b.interface("org.freedesktop.DBus.Properties"))
                    .and_then(|b| b.member("PropertiesChanged"))
                    .and_then(|b| b.path(SENSOR_PATH))
                {
                    Ok(r) => r.build(),
                    Err(_) => return,
                };

                if let Ok(mut stream) =
                    zbus::MessageStream::for_match_rule(rule, &conn, Some(32)).await
                {
                    while !stop.is_stopped() {
                        let next =
                            futures_lite::future::or(async { Some(stream.next().await) }, async {
                                stop.wait_stopped().await;
                                None
                            })
                            .await;
                        let Some(Some(Ok(_))) = next else {
                            break;
                        };
                        while futures_lite::future::poll_once(stream.next())
                            .await
                            .is_some()
                        {}
                        if stop.is_stopped() {
                            break;
                        }
                        if let Ok(props) =
                            get_all(&conn, SENSOR_DEST, SENSOR_PATH, SENSOR_IFACE).await
                        {
                            let snap = parse_sensor_props(&props);
                            if snap != last {
                                last = snap;
                                let _ = sender.send(snap);
                            }
                        }
                    }
                }

                if claimed_accel {
                    let _ = conn
                        .call_method(
                            Some(SENSOR_DEST),
                            SENSOR_PATH,
                            Some(SENSOR_IFACE),
                            "ReleaseAccelerometer",
                            &(),
                        )
                        .await;
                }
                if claimed_light {
                    let _ = conn
                        .call_method(
                            Some(SENSOR_DEST),
                            SENSOR_PATH,
                            Some(SENSOR_IFACE),
                            "ReleaseLight",
                            &(),
                        )
                        .await;
                }
                if claimed_prox {
                    let _ = conn
                        .call_method(
                            Some(SENSOR_DEST),
                            SENSOR_PATH,
                            Some(SENSOR_IFACE),
                            "ReleaseProximity",
                            &(),
                        )
                        .await;
                }
            });
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iio_sensor_proxy_properties() {
        let mut props: HashMap<String, OwnedValue> = HashMap::new();
        props.insert("HasAccelerometer".into(), OwnedValue::from(true));
        props.insert(
            "AccelerometerOrientation".into(),
            OwnedValue::try_from(zbus::zvariant::Value::from("left-up")).unwrap(),
        );
        props.insert("HasAmbientLight".into(), OwnedValue::from(true));
        props.insert("LightLevel".into(), OwnedValue::from(320.5f64));
        props.insert(
            "LightLevelUnit".into(),
            OwnedValue::try_from(zbus::zvariant::Value::from("lux")).unwrap(),
        );
        props.insert("HasProximity".into(), OwnedValue::from(true));
        props.insert("ProximityNear".into(), OwnedValue::from(true));

        let snap = parse_sensor_props(&props);
        assert!(snap.has_accelerometer);
        assert_eq!(snap.accelerometer_orientation, SensorOrientation::LeftUp);
        assert!(snap.has_ambient_light);
        assert_eq!(snap.light_level, 320.5);
        assert_eq!(snap.light_level_unit, LightLevelUnit::Lux);
        assert!(snap.has_proximity);
        assert!(snap.proximity_near);
    }
}
