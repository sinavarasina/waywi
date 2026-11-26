//! systemd D-Bus domain clients and subscriptions:
//! - `org.freedesktop.systemd1.Manager` (user & system unit monitoring + lifecycle control)
//! - `org.freedesktop.timedate1` (timezone, NTP sync, system time)
//! - `org.freedesktop.hostname1` (hostname, chassis, OS/hardware metadata)
//! - `org.freedesktop.locale1` (system locale, vconsole & X11 keyboard configuration)

use std::collections::HashMap;

use futures_lite::StreamExt;
use zbus::zvariant::OwnedValue;

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{
    DBUS_SLOW_TIMEOUT, DbusActionLane, DbusActionReply, DbusClient, DbusSharedBus, LaneActionSpec,
    connect_shared_bus,
};
use super::types::{Dbus, DbusBus};
use super::worker::{DbusWorkerHandle, ServiceWatchWorker};
use super::zvariant_helpers::{dict_bool, dict_str, dict_str_vec, dict_u64, get_all};

const SYSTEMD_DEST: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MGR_IFACE: &str = "org.freedesktop.systemd1.Manager";
const SYSTEMD_UNIT_IFACE: &str = "org.freedesktop.systemd1.Unit";

const TIMEDATE_DEST: &str = "org.freedesktop.timedate1";
const TIMEDATE_PATH: &str = "/org/freedesktop/timedate1";
const TIMEDATE_IFACE: &str = "org.freedesktop.timedate1";

const HOSTNAME_DEST: &str = "org.freedesktop.hostname1";
const HOSTNAME_PATH: &str = "/org/freedesktop/hostname1";
const HOSTNAME_IFACE: &str = "org.freedesktop.hostname1";

const LOCALE_DEST: &str = "org.freedesktop.locale1";
const LOCALE_PATH: &str = "/org/freedesktop/locale1";
const LOCALE_IFACE: &str = "org.freedesktop.locale1";

/// Summary of a loaded systemd unit from `org.freedesktop.systemd1.Manager.ListUnits`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SystemdUnitInfo {
    pub name: String,
    pub description: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub following: String,
    pub object_path: String,
    pub job_id: u32,
    pub job_type: String,
}

impl SystemdUnitInfo {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active_state == "active"
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.active_state == "failed"
    }
}

/// Snapshot of loaded systemd units on a given bus (`Session` = user manager, `System` = system manager).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SystemdUnitsSnapshot {
    pub bus: DbusBus,
    pub units: Vec<SystemdUnitInfo>,
}

impl SystemdUnitsSnapshot {
    #[must_use]
    pub fn unit(&self, name: &str) -> Option<&SystemdUnitInfo> {
        self.units.iter().find(|u| u.name == name)
    }

    pub fn failed_units(&self) -> impl Iterator<Item = &SystemdUnitInfo> {
        self.units.iter().filter(|u| u.is_failed())
    }
}

/// Snapshot of `org.freedesktop.timedate1` timezone and NTP state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TimedateSnapshot {
    pub timezone: String,
    pub local_rtc: bool,
    pub can_ntp: bool,
    pub ntp: bool,
    pub ntp_synchronized: bool,
    pub time_usec: u64,
}

/// Snapshot of `org.freedesktop.hostname1` host and hardware metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostnameSnapshot {
    pub hostname: String,
    pub static_hostname: String,
    pub pretty_hostname: String,
    pub icon_name: String,
    pub chassis: String,
    pub deployment: String,
    pub location: String,
    pub kernel_name: String,
    pub kernel_release: String,
    pub operating_system_pretty_name: String,
    pub hardware_vendor: String,
    pub hardware_model: String,
}

/// Snapshot of `org.freedesktop.locale1` locale and keyboard settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocaleSnapshot {
    pub locale: Vec<String>,
    pub vconsole_keymap: String,
    pub vconsole_keymap_toggle: String,
    pub x11_layout: String,
    pub x11_model: String,
    pub x11_variant: String,
    pub x11_options: String,
}

impl Dbus {
    /// Subscribes to systemd unit changes on `bus` (`DbusBus::Session` for user units,
    /// `DbusBus::System` for system units) via `Manager.Subscribe()` and unit/job signals.
    pub fn systemd_units<M: 'static>(
        bus: DbusBus,
        map: impl Fn(SystemdUnitsSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusSystemdUnits {
                bus,
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Subscribes to `org.freedesktop.timedate1` timezone and NTP changes on the system bus.
    pub fn timedate<M: 'static>(map: impl Fn(TimedateSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusTimedate {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for `org.freedesktop.systemd1.Manager`.
#[derive(Clone, Copy)]
pub struct SystemdClient<'a>(pub(super) &'a DbusClient);

impl SystemdClient<'_> {
    /// Returns the latest cached [`SystemdUnitsSnapshot`] for `bus` from an active
    /// [`Dbus::systemd_units`] subscription.
    #[must_use]
    pub fn cached_snapshot(&self, bus: DbusBus) -> Option<SystemdUnitsSnapshot> {
        let lock = match bus {
            DbusBus::Session => &self.0.cache.systemd_user,
            DbusBus::System => &self.0.cache.systemd_system,
        };
        lock.read().ok().and_then(|g| g.clone())
    }

    /// Lists loaded systemd units synchronously on `bus`.
    pub fn list_units_blocking(&self, bus: DbusBus) -> Result<SystemdUnitsSnapshot> {
        let conn = self.0.async_connection(bus)?;
        futures_lite::future::block_on(read_systemd_units(&conn, bus))
    }

    /// Reads the `ActiveState` and `SubState` of a single unit synchronously.
    pub fn unit_state_blocking(&self, bus: DbusBus, unit: &str) -> Result<(String, String)> {
        if unit.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "systemd unit name cannot be empty".into(),
            ));
        }
        let conn = self.0.async_connection(bus)?;
        let unit_owned = unit.to_owned();
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(SYSTEMD_DEST),
                    SYSTEMD_PATH,
                    Some(SYSTEMD_MGR_IFACE),
                    "LoadUnit",
                    &(unit_owned.as_str(),),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("systemd LoadUnit failed: {e}")))?;
            let path: zbus::zvariant::OwnedObjectPath = reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            let props = get_all(&conn, SYSTEMD_DEST, path.as_str(), SYSTEMD_UNIT_IFACE).await?;
            Ok((
                dict_str(&props, "ActiveState").unwrap_or_default(),
                dict_str(&props, "SubState").unwrap_or_default(),
            ))
        })
    }

    /// Starts `unit` (`mode` defaults to `"replace"`), emitting `DbusActionReply::ObjectPath(job)`.
    pub fn start_unit(&self, bus: DbusBus, unit: &str) -> Result<()> {
        self.unit_lifecycle_action(bus, unit, "StartUnit")
    }

    /// Stops `unit`, emitting `DbusActionReply::ObjectPath(job)`.
    pub fn stop_unit(&self, bus: DbusBus, unit: &str) -> Result<()> {
        self.unit_lifecycle_action(bus, unit, "StopUnit")
    }

    /// Restarts `unit`, emitting `DbusActionReply::ObjectPath(job)`.
    pub fn restart_unit(&self, bus: DbusBus, unit: &str) -> Result<()> {
        self.unit_lifecycle_action(bus, unit, "RestartUnit")
    }

    /// Reloads `unit`, emitting `DbusActionReply::ObjectPath(job)`.
    pub fn reload_unit(&self, bus: DbusBus, unit: &str) -> Result<()> {
        self.unit_lifecycle_action(bus, unit, "ReloadUnit")
    }

    /// Enables unit files (`EnableUnitFiles`).
    pub fn enable_units(
        &self,
        bus: DbusBus,
        units: &[&str],
        runtime: bool,
        force: bool,
    ) -> Result<()> {
        if units.is_empty() {
            return Err(WaywiError::DbusError("units slice cannot be empty".into()));
        }
        let owned: Vec<String> = units.iter().map(|s| (*s).to_owned()).collect();
        let target = owned.join(",");
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                bus,
                "systemd1.EnableUnitFiles",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
                conn.call_method(
                    Some(SYSTEMD_DEST),
                    SYSTEMD_PATH,
                    Some(SYSTEMD_MGR_IFACE),
                    "EnableUnitFiles",
                    &(refs, runtime, force),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("systemd EnableUnitFiles failed: {e}"))
                })?;
                Ok(())
            },
        )
    }

    /// Disables unit files (`DisableUnitFiles`).
    pub fn disable_units(&self, bus: DbusBus, units: &[&str], runtime: bool) -> Result<()> {
        if units.is_empty() {
            return Err(WaywiError::DbusError("units slice cannot be empty".into()));
        }
        let owned: Vec<String> = units.iter().map(|s| (*s).to_owned()).collect();
        let target = owned.join(",");
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                bus,
                "systemd1.DisableUnitFiles",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
                conn.call_method(
                    Some(SYSTEMD_DEST),
                    SYSTEMD_PATH,
                    Some(SYSTEMD_MGR_IFACE),
                    "DisableUnitFiles",
                    &(refs, runtime),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("systemd DisableUnitFiles failed: {e}"))
                })?;
                Ok(())
            },
        )
    }

    /// Triggers `org.freedesktop.systemd1.Manager.Reload` (`daemon-reload`).
    pub fn daemon_reload(&self, bus: DbusBus) -> Result<()> {
        self.0.call_lane_method_unit(
            DbusActionLane::Desktop,
            bus,
            SYSTEMD_DEST,
            SYSTEMD_PATH,
            SYSTEMD_MGR_IFACE,
            "Reload",
        )
    }

    fn unit_lifecycle_action(&self, bus: DbusBus, unit: &str, method: &'static str) -> Result<()> {
        if unit.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "systemd unit name cannot be empty".into(),
            ));
        }
        let unit_owned = unit.to_owned();
        let target = unit_owned.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                bus,
                format!("systemd1.{method}"),
                target,
            )
            .with_dedup(format!("systemd:{bus:?}:{method}:{unit_owned}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let reply = conn
                    .call_method(
                        Some(SYSTEMD_DEST),
                        SYSTEMD_PATH,
                        Some(SYSTEMD_MGR_IFACE),
                        method,
                        &(unit_owned.as_str(), "replace"),
                    )
                    .await
                    .map_err(|e| {
                        WaywiError::DbusError(format!("systemd {method}({unit_owned}) failed: {e}"))
                    })?;
                let job: zbus::zvariant::OwnedObjectPath = reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                Ok(DbusActionReply::ObjectPath(job.to_string()))
            },
        )
    }
}

/// Zero-cost domain handle for `org.freedesktop.timedate1`.
#[derive(Clone, Copy)]
pub struct TimedateClient<'a>(pub(super) &'a DbusClient);

impl TimedateClient<'_> {
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<TimedateSnapshot> {
        self.0.cache.timedate.read().ok().and_then(|g| g.clone())
    }

    pub fn snapshot_blocking(&self) -> Result<TimedateSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let props = get_all(&conn, TIMEDATE_DEST, TIMEDATE_PATH, TIMEDATE_IFACE).await?;
            Ok(parse_timedate_props(&props))
        })
    }

    pub fn list_timezones_blocking(&self) -> Result<Vec<String>> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(TIMEDATE_DEST),
                    TIMEDATE_PATH,
                    Some(TIMEDATE_IFACE),
                    "ListTimezones",
                    &(),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("timedate1.ListTimezones failed: {e}"))
                })?;
            reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))
        })
    }

    pub fn set_timezone(&self, timezone: &str, interactive: bool) -> Result<()> {
        if timezone.trim().is_empty() {
            return Err(WaywiError::DbusError("timezone cannot be empty".into()));
        }
        let tz = timezone.to_owned();
        let target = tz.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "timedate1.SetTimezone",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(TIMEDATE_DEST),
                    TIMEDATE_PATH,
                    Some(TIMEDATE_IFACE),
                    "SetTimezone",
                    &(tz.as_str(), interactive),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("timedate1.SetTimezone failed: {e}")))?;
                Ok(())
            },
        )
    }

    pub fn set_ntp(&self, enabled: bool, interactive: bool) -> Result<()> {
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "timedate1.SetNTP",
                enabled.to_string(),
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(TIMEDATE_DEST),
                    TIMEDATE_PATH,
                    Some(TIMEDATE_IFACE),
                    "SetNTP",
                    &(enabled, interactive),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("timedate1.SetNTP failed: {e}")))?;
                Ok(())
            },
        )
    }

    pub fn set_local_rtc(
        &self,
        local_rtc: bool,
        fix_system: bool,
        interactive: bool,
    ) -> Result<()> {
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "timedate1.SetLocalRTC",
                local_rtc.to_string(),
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(TIMEDATE_DEST),
                    TIMEDATE_PATH,
                    Some(TIMEDATE_IFACE),
                    "SetLocalRTC",
                    &(local_rtc, fix_system, interactive),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("timedate1.SetLocalRTC failed: {e}")))?;
                Ok(())
            },
        )
    }

    pub fn set_time(&self, usec_utc: i64, relative: bool, interactive: bool) -> Result<()> {
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "timedate1.SetTime",
                usec_utc.to_string(),
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(TIMEDATE_DEST),
                    TIMEDATE_PATH,
                    Some(TIMEDATE_IFACE),
                    "SetTime",
                    &(usec_utc, relative, interactive),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("timedate1.SetTime failed: {e}")))?;
                Ok(())
            },
        )
    }
}

/// Zero-cost domain handle for `org.freedesktop.hostname1`.
#[derive(Clone, Copy)]
pub struct HostnameClient<'a>(pub(super) &'a DbusClient);

impl HostnameClient<'_> {
    pub fn snapshot_blocking(&self) -> Result<HostnameSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let props = get_all(&conn, HOSTNAME_DEST, HOSTNAME_PATH, HOSTNAME_IFACE).await?;
            Ok(parse_hostname_props(&props))
        })
    }

    pub fn set_pretty_hostname(&self, name: &str, interactive: bool) -> Result<()> {
        let owned = name.to_owned();
        let target = owned.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "hostname1.SetPrettyHostname",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(HOSTNAME_DEST),
                    HOSTNAME_PATH,
                    Some(HOSTNAME_IFACE),
                    "SetPrettyHostname",
                    &(owned.as_str(), interactive),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("hostname1.SetPrettyHostname failed: {e}"))
                })?;
                Ok(())
            },
        )
    }

    pub fn set_static_hostname(&self, name: &str, interactive: bool) -> Result<()> {
        let owned = name.to_owned();
        let target = owned.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "hostname1.SetStaticHostname",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(HOSTNAME_DEST),
                    HOSTNAME_PATH,
                    Some(HOSTNAME_IFACE),
                    "SetStaticHostname",
                    &(owned.as_str(), interactive),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("hostname1.SetStaticHostname failed: {e}"))
                })?;
                Ok(())
            },
        )
    }
}

/// Zero-cost domain handle for `org.freedesktop.locale1`.
#[derive(Clone, Copy)]
pub struct LocaleClient<'a>(pub(super) &'a DbusClient);

impl LocaleClient<'_> {
    pub fn snapshot_blocking(&self) -> Result<LocaleSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let props = get_all(&conn, LOCALE_DEST, LOCALE_PATH, LOCALE_IFACE).await?;
            Ok(parse_locale_props(&props))
        })
    }

    pub fn set_x11_keyboard(
        &self,
        layout: &str,
        model: &str,
        variant: &str,
        options: &str,
        convert: bool,
        interactive: bool,
    ) -> Result<()> {
        let layout = layout.to_owned();
        let model = model.to_owned();
        let variant = variant.to_owned();
        let options = options.to_owned();
        let target = layout.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "locale1.SetX11Keyboard",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(LOCALE_DEST),
                    LOCALE_PATH,
                    Some(LOCALE_IFACE),
                    "SetX11Keyboard",
                    &(
                        layout.as_str(),
                        model.as_str(),
                        variant.as_str(),
                        options.as_str(),
                        convert,
                        interactive,
                    ),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("locale1.SetX11Keyboard failed: {e}"))
                })?;
                Ok(())
            },
        )
    }

    pub fn set_locale(&self, locales: &[&str], interactive: bool) -> Result<()> {
        let owned: Vec<String> = locales.iter().map(|s| (*s).to_owned()).collect();
        let target = owned.join(",");
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "locale1.SetLocale",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
                conn.call_method(
                    Some(LOCALE_DEST),
                    LOCALE_PATH,
                    Some(LOCALE_IFACE),
                    "SetLocale",
                    &(refs, interactive),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("locale1.SetLocale failed: {e}")))?;
                Ok(())
            },
        )
    }
}

type RawUnitTuple = (
    String,
    String,
    String,
    String,
    String,
    String,
    zbus::zvariant::OwnedObjectPath,
    u32,
    String,
    zbus::zvariant::OwnedObjectPath,
);

async fn read_systemd_units(conn: &zbus::Connection, bus: DbusBus) -> Result<SystemdUnitsSnapshot> {
    let reply = conn
        .call_method(
            Some(SYSTEMD_DEST),
            SYSTEMD_PATH,
            Some(SYSTEMD_MGR_IFACE),
            "ListUnits",
            &(),
        )
        .await
        .map_err(|e| WaywiError::DbusError(format!("systemd ListUnits failed: {e}")))?;
    let raw: Vec<RawUnitTuple> = reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
    let mut units: Vec<SystemdUnitInfo> = raw
        .into_iter()
        .map(|t| SystemdUnitInfo {
            name: t.0,
            description: t.1,
            load_state: t.2,
            active_state: t.3,
            sub_state: t.4,
            following: t.5,
            object_path: t.6.to_string(),
            job_id: t.7,
            job_type: t.8,
        })
        .collect();
    units.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(SystemdUnitsSnapshot { bus, units })
}

pub(crate) struct SystemdUnitsWorker(#[allow(dead_code)] DbusWorkerHandle);

impl SystemdUnitsWorker {
    pub(crate) fn spawn(
        bus: DbusBus,
        sender: calloop::channel::Sender<SystemdUnitsSnapshot>,
        shared: DbusSharedBus,
    ) -> Self {
        Self(DbusWorkerHandle::spawn("waywi-dbus-systemd", move |stop| {
            let Ok(conn) = connect_shared_bus(bus, &shared) else {
                return;
            };
            futures_lite::future::block_on(async move {
                let subscribed = conn
                    .call_method(
                        Some(SYSTEMD_DEST),
                        SYSTEMD_PATH,
                        Some(SYSTEMD_MGR_IFACE),
                        "Subscribe",
                        &(),
                    )
                    .await
                    .is_ok();

                let mut last = read_systemd_units(&conn, bus).await.ok();
                if let Some(ref snap) = last {
                    let _ = sender.send(snap.clone());
                }

                let rule = match zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender(SYSTEMD_DEST)
                    .and_then(|b| b.interface(SYSTEMD_MGR_IFACE))
                {
                    Ok(r) => r.build(),
                    Err(_) => return,
                };

                if let Ok(mut stream) =
                    zbus::MessageStream::for_match_rule(rule, &conn, Some(64)).await
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
                        if let Ok(snap) = read_systemd_units(&conn, bus).await
                            && last.as_ref() != Some(&snap)
                        {
                            last = Some(snap.clone());
                            let _ = sender.send(snap);
                        }
                    }
                }

                if subscribed {
                    let _ = conn
                        .call_method(
                            Some(SYSTEMD_DEST),
                            SYSTEMD_PATH,
                            Some(SYSTEMD_MGR_IFACE),
                            "Unsubscribe",
                            &(),
                        )
                        .await;
                }
            });
        }))
    }
}

pub(crate) fn spawn_timedate_worker(
    sender: calloop::channel::Sender<TimedateSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-timedate",
        Some(TIMEDATE_DEST),
        None,
        sender,
        shared,
        |conn| async move {
            let props = get_all(&conn, TIMEDATE_DEST, TIMEDATE_PATH, TIMEDATE_IFACE)
                .await
                .ok()?;
            Some(parse_timedate_props(&props))
        },
    )
}

pub(crate) fn parse_timedate_props(props: &HashMap<String, OwnedValue>) -> TimedateSnapshot {
    TimedateSnapshot {
        timezone: dict_str(props, "Timezone").unwrap_or_default(),
        local_rtc: dict_bool(props, "LocalRTC").unwrap_or(false),
        can_ntp: dict_bool(props, "CanNTP").unwrap_or(false),
        ntp: dict_bool(props, "NTP").unwrap_or(false),
        ntp_synchronized: dict_bool(props, "NTPSynchronized").unwrap_or(false),
        time_usec: dict_u64(props, "TimeUSec").unwrap_or(0),
    }
}

pub(crate) fn parse_hostname_props(props: &HashMap<String, OwnedValue>) -> HostnameSnapshot {
    HostnameSnapshot {
        hostname: dict_str(props, "Hostname").unwrap_or_default(),
        static_hostname: dict_str(props, "StaticHostname").unwrap_or_default(),
        pretty_hostname: dict_str(props, "PrettyHostname").unwrap_or_default(),
        icon_name: dict_str(props, "IconName").unwrap_or_default(),
        chassis: dict_str(props, "Chassis").unwrap_or_default(),
        deployment: dict_str(props, "Deployment").unwrap_or_default(),
        location: dict_str(props, "Location").unwrap_or_default(),
        kernel_name: dict_str(props, "KernelName").unwrap_or_default(),
        kernel_release: dict_str(props, "KernelRelease").unwrap_or_default(),
        operating_system_pretty_name: dict_str(props, "OperatingSystemPrettyName")
            .unwrap_or_default(),
        hardware_vendor: dict_str(props, "HardwareVendor").unwrap_or_default(),
        hardware_model: dict_str(props, "HardwareModel").unwrap_or_default(),
    }
}

pub(crate) fn parse_locale_props(props: &HashMap<String, OwnedValue>) -> LocaleSnapshot {
    LocaleSnapshot {
        locale: dict_str_vec(props, "Locale"),
        vconsole_keymap: dict_str(props, "VConsoleKeymap").unwrap_or_default(),
        vconsole_keymap_toggle: dict_str(props, "VConsoleKeymapToggle").unwrap_or_default(),
        x11_layout: dict_str(props, "X11Layout").unwrap_or_default(),
        x11_model: dict_str(props, "X11Model").unwrap_or_default(),
        x11_variant: dict_str(props, "X11Variant").unwrap_or_default(),
        x11_options: dict_str(props, "X11Options").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn parses_timedate_hostname_and_locale_properties() {
        let mut td = HashMap::new();
        td.insert(
            "Timezone".into(),
            OwnedValue::try_from(Value::from("Asia/Jakarta")).unwrap(),
        );
        td.insert("NTP".into(), OwnedValue::from(true));
        td.insert("NTPSynchronized".into(), OwnedValue::from(true));
        td.insert(
            "TimeUSec".into(),
            OwnedValue::from(1_700_000_000_000_000u64),
        );
        let td_snap = parse_timedate_props(&td);
        assert_eq!(td_snap.timezone, "Asia/Jakarta");
        assert!(td_snap.ntp && td_snap.ntp_synchronized);

        let mut hn = HashMap::new();
        hn.insert(
            "Hostname".into(),
            OwnedValue::try_from(Value::from("waywi-box")).unwrap(),
        );
        hn.insert(
            "Chassis".into(),
            OwnedValue::try_from(Value::from("laptop")).unwrap(),
        );
        let hn_snap = parse_hostname_props(&hn);
        assert_eq!(hn_snap.hostname, "waywi-box");
        assert_eq!(hn_snap.chassis, "laptop");

        let mut loc = HashMap::new();
        loc.insert(
            "X11Layout".into(),
            OwnedValue::try_from(Value::from("us,id")).unwrap(),
        );
        let loc_snap = parse_locale_props(&loc);
        assert_eq!(loc_snap.x11_layout, "us,id");
    }
}
