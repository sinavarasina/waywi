//! System power profile monitoring and switching (`net.hadess.PowerProfiles` /
//! `org.freedesktop.UPower.PowerProfiles`).

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{DbusClient, DbusSharedBus, connect_shared_bus};
use super::types::{Dbus, DbusBus};
use super::worker::AsyncStopSignal;

const POWER_PROFILE_TARGETS: &[(&str, &str)] = &[
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
    ),
];

/// Power profile mode reported by `power-profiles-daemon` or `tuned-ppd`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum PowerProfileKind {
    PowerSaver,
    #[default]
    Balanced,
    Performance,
    Custom(String),
}

impl PowerProfileKind {
    /// Parses a D-Bus profile string (`"power-saver"`, `"balanced"`, `"performance"`).
    #[must_use]
    pub fn from_dbus_str(s: &str) -> Self {
        match s {
            "power-saver" => Self::PowerSaver,
            "balanced" => Self::Balanced,
            "performance" => Self::Performance,
            other => Self::Custom(other.to_owned()),
        }
    }

    /// Returns the canonical D-Bus identifier for this profile.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::PowerSaver => "power-saver",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
            Self::Custom(s) => s.as_str(),
        }
    }
}

/// Active application hold preventing or overriding automatic power profile changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerProfileHold {
    pub application_id: String,
    pub profile: PowerProfileKind,
    pub reason: String,
}

/// Snapshot of system power profile state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PowerProfileSnapshot {
    pub active_profile: PowerProfileKind,
    pub performance_degraded: Option<String>,
    pub performance_inhibited: Option<String>,
    pub profiles: Vec<PowerProfileKind>,
    pub active_holds: Vec<PowerProfileHold>,
    pub service_name: String,
}

impl Dbus {
    /// Subscribes to system power profile changes on the system D-Bus.
    pub fn power_profiles<M: 'static>(
        map: impl Fn(PowerProfileSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusPowerProfiles {
                key: "dbus:power-profiles".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Reads the current [`PowerProfileSnapshot`] from the system bus.
    pub fn power_profile_snapshot(&self) -> Result<PowerProfileSnapshot> {
        let conn = self.connection(DbusBus::System)?;
        let mut last_err = None;
        for &(dest, path) in POWER_PROFILE_TARGETS {
            match conn.call_method(
                Some(dest),
                path,
                Some("org.freedesktop.DBus.Properties"),
                "GetAll",
                &(dest,),
            ) {
                Ok(reply) => {
                    let props: HashMap<String, OwnedValue> = reply
                        .body()
                        .deserialize()
                        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                    return Ok(parse_power_profile_props(dest, &props));
                }
                Err(err) => last_err = Some(err),
            }
        }
        Err(WaywiError::DbusError(format!(
            "power profiles service unavailable: {}",
            last_err.map_or_else(|| "unknown error".to_owned(), |e| e.to_string())
        )))
    }

    /// Switches the active system power profile (`"power-saver"`, `"balanced"`, `"performance"`).
    pub fn set_power_profile(&self, profile: PowerProfileKind) -> Result<()> {
        let mut last_err = None;
        for &(dest, path) in POWER_PROFILE_TARGETS {
            match self.set_property(
                DbusBus::System,
                dest,
                path,
                dest,
                "ActiveProfile",
                Value::from(profile.as_str()),
            ) {
                Ok(()) => return Ok(()),
                Err(err) => last_err = Some(err),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            WaywiError::DbusError("power profiles service unavailable".to_owned())
        }))
    }

    /// Acquires a temporary power profile hold and returns the hold cookie for [`Self::release_power_profile`].
    pub fn hold_power_profile(
        &self,
        profile: PowerProfileKind,
        reason: &str,
        application_id: &str,
    ) -> Result<u32> {
        let conn = self.connection(DbusBus::System)?;
        let mut last_err = None;
        for &(dest, path) in POWER_PROFILE_TARGETS {
            match conn.call_method(
                Some(dest),
                path,
                Some(dest),
                "HoldProfile",
                &(profile.as_str(), reason, application_id),
            ) {
                Ok(reply) => {
                    let cookie: u32 = reply
                        .body()
                        .deserialize()
                        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                    return Ok(cookie);
                }
                Err(err) => last_err = Some(err),
            }
        }
        Err(WaywiError::DbusError(format!(
            "HoldProfile failed: {}",
            last_err.map_or_else(|| "unknown error".to_owned(), |e| e.to_string())
        )))
    }

    /// Releases a previously acquired power profile hold cookie.
    pub fn release_power_profile(&self, cookie: u32) -> Result<()> {
        let conn = self.connection(DbusBus::System)?;
        let mut last_err = None;
        for &(dest, path) in POWER_PROFILE_TARGETS {
            match conn.call_method(Some(dest), path, Some(dest), "ReleaseProfile", &(cookie,)) {
                Ok(_) => return Ok(()),
                Err(err) => last_err = Some(err),
            }
        }
        Err(WaywiError::DbusError(format!(
            "ReleaseProfile failed: {}",
            last_err.map_or_else(|| "unknown error".to_owned(), |e| e.to_string())
        )))
    }
}

pub(crate) fn parse_power_profile_props(
    service_name: &str,
    props: &HashMap<String, OwnedValue>,
) -> PowerProfileSnapshot {
    let active_profile = props
        .get("ActiveProfile")
        .and_then(|v| <&str>::try_from(&**v).ok())
        .map(PowerProfileKind::from_dbus_str)
        .unwrap_or_default();

    let performance_degraded = non_empty_str_prop(props.get("PerformanceDegraded"));
    let performance_inhibited = non_empty_str_prop(props.get("PerformanceInhibited"));

    let mut profiles = Vec::new();
    if let Some(val) = props.get("Profiles")
        && let Ok(entries) = Vec::<HashMap<String, OwnedValue>>::try_from(val.clone())
    {
        for entry in entries {
            if let Some(p) = entry
                .get("Profile")
                .and_then(|v| <&str>::try_from(&**v).ok())
            {
                profiles.push(PowerProfileKind::from_dbus_str(p));
            }
        }
    }

    let mut active_holds = Vec::new();
    if let Some(val) = props.get("ActiveProfileHolds")
        && let Ok(entries) = Vec::<HashMap<String, OwnedValue>>::try_from(val.clone())
    {
        for entry in entries {
            let application_id = entry
                .get("ApplicationId")
                .and_then(|v| <&str>::try_from(&**v).ok())
                .unwrap_or_default()
                .to_owned();
            let reason = entry
                .get("Reason")
                .and_then(|v| <&str>::try_from(&**v).ok())
                .unwrap_or_default()
                .to_owned();
            if let Some(p) = entry
                .get("Profile")
                .and_then(|v| <&str>::try_from(&**v).ok())
            {
                active_holds.push(PowerProfileHold {
                    application_id,
                    profile: PowerProfileKind::from_dbus_str(p),
                    reason,
                });
            }
        }
    }

    PowerProfileSnapshot {
        active_profile,
        performance_degraded,
        performance_inhibited,
        profiles,
        active_holds,
        service_name: service_name.to_owned(),
    }
}

fn non_empty_str_prop(val: Option<&OwnedValue>) -> Option<String> {
    val.and_then(|v| <&str>::try_from(&**v).ok())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(crate) struct PowerProfilesWorker {
    stop: AsyncStopSignal,
    join: Option<std::thread::JoinHandle<()>>,
}

impl PowerProfilesWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<PowerProfileSnapshot>,
        shared: DbusSharedBus,
    ) -> Self {
        let stop = AsyncStopSignal::new();
        let stop_sig = stop.clone();
        let join = std::thread::Builder::new()
            .name("waywi-dbus-ppd".to_owned())
            .spawn(move || {
                let Ok(conn) = connect_shared_bus(DbusBus::System, &shared) else {
                    return;
                };
                futures_lite::future::block_on(async move {
                    if let Some((dest, path, snap)) = fetch_async_snapshot(&conn).await {
                        let _ = sender.send(snap);
                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .interface("org.freedesktop.DBus.Properties")
                            .and_then(|b| b.member("PropertiesChanged"))
                            .and_then(|b| b.path(path))
                        else {
                            return;
                        };
                        let Ok(mut stream) =
                            zbus::MessageStream::for_match_rule(rule.build(), &conn, Some(32))
                                .await
                        else {
                            return;
                        };
                        use futures_lite::StreamExt;
                        while !stop_sig.is_stopped() {
                            let next_msg = futures_lite::future::or(
                                async { Some(stream.next().await) },
                                async {
                                    stop_sig.wait_stopped().await;
                                    None
                                },
                            )
                            .await;
                            let Some(Some(Ok(_))) = next_msg else {
                                break;
                            };
                            if let Ok(reply) = conn
                                .call_method(
                                    Some(dest),
                                    path,
                                    Some("org.freedesktop.DBus.Properties"),
                                    "GetAll",
                                    &(dest,),
                                )
                                .await
                                && let Ok(props) =
                                    reply.body().deserialize::<HashMap<String, OwnedValue>>()
                            {
                                let _ = sender.send(parse_power_profile_props(dest, &props));
                            }
                        }
                    } else {
                        stop_sig.wait_stopped().await;
                    }
                });
            })
            .ok();
        Self { stop, join }
    }
}

impl Drop for PowerProfilesWorker {
    fn drop(&mut self) {
        self.stop.trigger();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

async fn fetch_async_snapshot(
    conn: &zbus::Connection,
) -> Option<(&'static str, &'static str, PowerProfileSnapshot)> {
    for &(dest, path) in POWER_PROFILE_TARGETS {
        if let Ok(reply) = conn
            .call_method(
                Some(dest),
                path,
                Some("org.freedesktop.DBus.Properties"),
                "GetAll",
                &(dest,),
            )
            .await
            && let Ok(props) = reply.body().deserialize::<HashMap<String, OwnedValue>>()
        {
            return Some((dest, path, parse_power_profile_props(dest, &props)));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_power_profile_properties_cleanly() {
        let mut props = HashMap::new();
        props.insert(
            "ActiveProfile".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from("performance")),
        );
        props.insert(
            "PerformanceDegraded".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from("high-operating-temperature")),
        );
        props.insert(
            "PerformanceInhibited".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from("")),
        );

        let snap = parse_power_profile_props("net.hadess.PowerProfiles", &props);
        assert_eq!(snap.active_profile, PowerProfileKind::Performance);
        assert_eq!(
            snap.performance_degraded.as_deref(),
            Some("high-operating-temperature")
        );
        assert_eq!(snap.performance_inhibited, None);
    }
}
