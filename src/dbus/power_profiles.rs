//! System power profile monitoring and switching (`net.hadess.PowerProfiles` /
//! `org.freedesktop.UPower.PowerProfiles`).

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{
    DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec, connect_shared_bus,
};
use super::types::{Dbus, DbusBus};
use super::worker::DbusWorkerHandle;
use super::zvariant_helpers::get_all;

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
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for `power-profiles-daemon` / `tuned-ppd` queries and profile switching.
#[derive(Clone, Copy)]
pub struct PowerProfilesClient<'a>(pub(super) &'a DbusClient);

impl PowerProfilesClient<'_> {
    /// Returns the latest cached [`PowerProfileSnapshot`] from an active [`Dbus::power_profiles`]
    /// subscription without issuing any D-Bus calls (0 ms).
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<PowerProfileSnapshot> {
        self.0
            .cache
            .power_profiles
            .read()
            .ok()
            .and_then(|g| g.clone())
    }

    /// Queries the system bus synchronously for the current [`PowerProfileSnapshot`].
    pub fn snapshot_blocking(&self) -> Result<PowerProfileSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let (_, _, snap) = fetch_power_profile_snapshot(&conn).await?;
            Ok(snap)
        })
    }

    /// Switches the active system power profile (`"power-saver"`, `"balanced"`, `"performance"`).
    pub fn set_profile(&self, profile: PowerProfileKind) -> Result<()> {
        let target = profile.as_str().to_owned();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "PowerProfiles.SetActiveProfile",
                target,
            ),
            move |conn| async move {
                let val_str = profile.as_str();
                let mut last_err = None;
                for &(dest, path) in POWER_PROFILE_TARGETS {
                    match conn
                        .call_method(
                            Some(dest),
                            path,
                            Some("org.freedesktop.DBus.Properties"),
                            "Set",
                            &(dest, "ActiveProfile", Value::from(val_str)),
                        )
                        .await
                    {
                        Ok(_) => return Ok(()),
                        Err(e) => last_err = Some(e),
                    }
                }
                Err(WaywiError::DbusError(format!(
                    "SetActiveProfile failed: {}",
                    last_err.map_or_else(
                        || "power profiles service unavailable".to_owned(),
                        |e| e.to_string(),
                    )
                )))
            },
        )
    }

    /// Acquires a temporary power profile hold synchronously and returns the hold cookie for
    /// [`Self::release_hold`].
    pub fn hold_blocking(
        &self,
        profile: PowerProfileKind,
        reason: &str,
        app_id: &str,
    ) -> Result<u32> {
        let conn = self.0.async_connection(DbusBus::System)?;
        let reason = reason.to_owned();
        let app_id = app_id.to_owned();
        futures_lite::future::block_on(async move {
            let mut last_err = None;
            for &(dest, path) in POWER_PROFILE_TARGETS {
                match conn
                    .call_method(
                        Some(dest),
                        path,
                        Some(dest),
                        "HoldProfile",
                        &(profile.as_str(), reason.as_str(), app_id.as_str()),
                    )
                    .await
                {
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
        })
    }

    /// Releases a previously acquired power profile hold cookie.
    pub fn release_hold(&self, cookie: u32) -> Result<()> {
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "PowerProfiles.ReleaseProfile",
                cookie.to_string(),
            ),
            move |conn| async move {
                let mut last_err = None;
                for &(dest, path) in POWER_PROFILE_TARGETS {
                    match conn
                        .call_method(Some(dest), path, Some(dest), "ReleaseProfile", &(cookie,))
                        .await
                    {
                        Ok(_) => return Ok(()),
                        Err(e) => last_err = Some(e),
                    }
                }
                Err(WaywiError::DbusError(format!(
                    "ReleaseProfile failed: {}",
                    last_err.map_or_else(
                        || "power profiles service unavailable".to_owned(),
                        |e| e.to_string(),
                    )
                )))
            },
        )
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

pub(crate) struct PowerProfilesWorker(#[allow(dead_code)] DbusWorkerHandle);

impl PowerProfilesWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<PowerProfileSnapshot>,
        shared: DbusSharedBus,
    ) -> Self {
        Self(DbusWorkerHandle::spawn("waywi-dbus-ppd", move |stop_sig| {
            let Ok(conn) = connect_shared_bus(DbusBus::System, &shared) else {
                return;
            };
            futures_lite::future::block_on(async move {
                if let Ok((dest, path, snap)) = fetch_power_profile_snapshot(&conn).await {
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
                        zbus::MessageStream::for_match_rule(rule.build(), &conn, Some(32)).await
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
                        let mut stream_closed = false;
                        while let Some(item) = futures_lite::future::poll_once(stream.next()).await
                        {
                            match item {
                                Some(Ok(_)) => {}
                                Some(Err(_)) | None => {
                                    stream_closed = true;
                                    break;
                                }
                            }
                        }
                        if let Ok(props) = get_all(&conn, dest, path, dest).await {
                            let _ = sender.send(parse_power_profile_props(dest, &props));
                        }
                        if stream_closed {
                            break;
                        }
                    }
                } else {
                    stop_sig.wait_stopped().await;
                }
            });
        }))
    }
}

async fn fetch_power_profile_snapshot(
    conn: &zbus::Connection,
) -> Result<(&'static str, &'static str, PowerProfileSnapshot)> {
    let mut last_err = None;
    for &(dest, path) in POWER_PROFILE_TARGETS {
        match get_all(conn, dest, path, dest).await {
            Ok(props) => return Ok((dest, path, parse_power_profile_props(dest, &props))),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err
        .unwrap_or_else(|| WaywiError::DbusError("power profiles service unavailable".to_owned())))
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
