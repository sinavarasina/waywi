//! systemd-logind session lock/idle state, backlight brightness, inhibitors, and power actions
//! (`org.freedesktop.login1`).

use std::collections::HashMap;
use std::env;

use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{
    DBUS_FAST_TIMEOUT, DBUS_SLOW_TIMEOUT, DbusActionLane, DbusClient, DbusSharedBus,
    LaneActionSpec, connect_shared_bus,
};
use super::types::{Dbus, DbusBus};
use super::worker::AsyncStopSignal;
use super::zvariant_helpers::{dict_bool, dict_str, dict_u64, get_all_async, get_all_blocking};

const LOGIN1_DEST: &str = "org.freedesktop.login1";
const LOGIN1_MANAGER_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";
const LOGIN1_SESSION_IFACE: &str = "org.freedesktop.login1.Session";
const LOGIN1_SESSION_AUTO: &str = "/org/freedesktop/login1/session/auto";

/// RAII sleep/idle/shutdown inhibitor guard returned by [`DbusClient::inhibit`].
///
/// Releasing or dropping this value closes the logind file descriptor lock automatically.
#[derive(Debug)]
pub struct LogindInhibitor {
    _fd: OwnedFd,
}

/// Availability and authorization status returned by `org.freedesktop.login1.Manager.Can*`
/// queries (`"yes"`, `"challenge"`, `"no"`, `"na"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PowerActionSupport {
    /// Supported and authorized without interactive authentication (`"yes"`).
    Yes,
    /// Supported, but requires interactive Polkit authentication (`"challenge"`).
    Challenge,
    /// Disallowed by Polkit policy or active inhibitor (`"no"`).
    No,
    /// Not supported by the hardware, kernel, swap configuration, or login1 daemon (`"na"`).
    #[default]
    NotAvailable,
}

impl PowerActionSupport {
    /// Parses a `systemd-logind` / `elogind` `Can*` reply string (`"yes"`, `"challenge"`,
    /// `"no"`, `"na"`).
    #[must_use]
    pub fn from_logind_str(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("yes") {
            Self::Yes
        } else if trimmed.eq_ignore_ascii_case("challenge") {
            Self::Challenge
        } else if trimmed.eq_ignore_ascii_case("no") {
            Self::No
        } else {
            Self::NotAvailable
        }
    }

    /// Returns `true` when the system supports the action (`Yes` or `Challenge`).
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Yes | Self::Challenge)
    }

    /// Returns `true` when invoking the action requires interactive Polkit authorization.
    #[must_use]
    pub const fn needs_auth(self) -> bool {
        matches!(self, Self::Challenge)
    }
}

/// Real-time hardware, kernel, and Polkit capability summary queried from
/// `org.freedesktop.login1.Manager.Can*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LogindPowerCapabilities {
    pub power_off: PowerActionSupport,
    pub reboot: PowerActionSupport,
    pub reboot_to_firmware_setup: PowerActionSupport,
    pub suspend: PowerActionSupport,
    pub hibernate: PowerActionSupport,
    pub hybrid_sleep: PowerActionSupport,
    pub suspend_then_hibernate: PowerActionSupport,
    pub soft_reboot: PowerActionSupport,
    pub halt: PowerActionSupport,
}

/// Snapshot of the active `systemd-logind` session and power transition state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogindSnapshot {
    pub session_id: String,
    pub session_path: String,
    pub locked_hint: bool,
    pub idle_hint: bool,
    pub idle_since_monotonic_us: u64,
    pub preparing_for_sleep: bool,
    pub preparing_for_shutdown: bool,
    pub block_inhibited: String,
    /// Availability of system power transitions reported by `org.freedesktop.login1.Manager.Can*`.
    pub power_capabilities: LogindPowerCapabilities,
}

/// Discrete event emitted by [`Dbus::logind`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogindEvent {
    Snapshot(LogindSnapshot),
    PreparingForSleep(bool),
    PreparingForShutdown(bool),
    LockRequested,
    UnlockRequested,
}

impl Dbus {
    /// Subscribes to `systemd-logind` session lock/idle and sleep/shutdown events.
    pub fn logind<M: 'static>(map: impl Fn(LogindEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusLogind {
                key: "dbus:logind".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Reads the current [`LogindSnapshot`] for the active user session from cache (0 ms) or the
    /// system bus.
    pub fn logind_snapshot(&self) -> Result<LogindSnapshot> {
        if let Ok(guard) = self.cache.logind.read()
            && let Some(ref snap) = *guard
        {
            return Ok(snap.clone());
        }
        let conn = self.connection(DbusBus::System)?;
        read_logind_snapshot_blocking(&conn)
    }

    /// Reads the current [`LogindPowerCapabilities`] (`CanPowerOff`, `CanReboot`, `CanSuspend`,
    /// `CanHibernate`, `CanHybridSleep`, `CanSuspendThenHibernate`, `CanRebootToFirmwareSetup`,
    /// `CanSoftReboot`, `CanHalt`) from cache (0 ms when [`Dbus::logind`] is active) or the
    /// system bus.
    pub fn logind_power_capabilities(&self) -> Result<LogindPowerCapabilities> {
        if let Ok(guard) = self.cache.logind.read()
            && let Some(ref snap) = *guard
        {
            return Ok(snap.power_capabilities);
        }
        let conn = self.connection(DbusBus::System)?;
        Ok(read_power_capabilities_blocking(&conn))
    }

    /// Sets device brightness via `org.freedesktop.login1.Session.SetBrightness` without root.
    pub fn set_backlight_brightness(
        &self,
        subsystem: &str,
        device_name: &str,
        brightness: u32,
    ) -> Result<()> {
        let sub = subsystem.to_owned();
        let dev = device_name.to_owned();
        let target = format!("{sub}:{dev}");
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Session.SetBrightness",
                target,
            ),
            move |conn| async move {
                let session_path = resolve_session_path_async(&conn)
                    .await
                    .unwrap_or_else(|| LOGIN1_SESSION_AUTO.to_owned());
                conn.call_method(
                    Some(LOGIN1_DEST),
                    session_path.as_str(),
                    Some(LOGIN1_SESSION_IFACE),
                    "SetBrightness",
                    &(sub.as_str(), dev.as_str(), brightness),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Session.SetBrightness failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Requests locking the active session via `org.freedesktop.login1.Session.Lock`.
    pub fn lock_session(&self) -> Result<()> {
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Session.Lock",
                LOGIN1_SESSION_AUTO,
            )
            .with_dedup("logind:lock"),
            move |conn| async move {
                let path = resolve_session_path_async(&conn)
                    .await
                    .unwrap_or_else(|| LOGIN1_SESSION_AUTO.to_owned());
                conn.call_method(
                    Some(LOGIN1_DEST),
                    path.as_str(),
                    Some(LOGIN1_SESSION_IFACE),
                    "Lock",
                    &(),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Session.Lock failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Requests unlocking the active session via `org.freedesktop.login1.Session.Unlock`.
    pub fn unlock_session(&self) -> Result<()> {
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Session.Unlock",
                LOGIN1_SESSION_AUTO,
            )
            .with_dedup("logind:unlock"),
            move |conn| async move {
                let path = resolve_session_path_async(&conn)
                    .await
                    .unwrap_or_else(|| LOGIN1_SESSION_AUTO.to_owned());
                conn.call_method(
                    Some(LOGIN1_DEST),
                    path.as_str(),
                    Some(LOGIN1_SESSION_IFACE),
                    "Unlock",
                    &(),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Session.Unlock failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Requests terminating (logging out) the active user session via
    /// `org.freedesktop.login1.Session.Terminate`.
    pub fn terminate_session(&self) -> Result<()> {
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Session.Terminate",
                LOGIN1_SESSION_AUTO,
            )
            .with_dedup("logind:terminate"),
            move |conn| async move {
                let path = resolve_session_path_async(&conn)
                    .await
                    .unwrap_or_else(|| LOGIN1_SESSION_AUTO.to_owned());
                conn.call_method(
                    Some(LOGIN1_DEST),
                    path.as_str(),
                    Some(LOGIN1_SESSION_IFACE),
                    "Terminate",
                    &(),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Session.Terminate failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Requests system suspend (Sleep / Suspend-to-RAM) via `org.freedesktop.login1.Manager.Suspend`.
    pub fn suspend(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("Suspend", interactive)
    }

    /// Requests system hibernate (Suspend-to-Disk) via `org.freedesktop.login1.Manager.Hibernate`.
    pub fn hibernate(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("Hibernate", interactive)
    }

    /// Requests hybrid sleep (Suspend-to-Both: writes state to disk and sleeps in RAM) via
    /// `org.freedesktop.login1.Manager.HybridSleep`.
    pub fn hybrid_sleep(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("HybridSleep", interactive)
    }

    /// Requests suspend-then-hibernate (sleeps in RAM first, then automatically transitions to
    /// hibernate after the configured delay) via `org.freedesktop.login1.Manager.SuspendThenHibernate`.
    pub fn suspend_then_hibernate(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("SuspendThenHibernate", interactive)
    }

    /// Requests system reboot via `org.freedesktop.login1.Manager.Reboot`.
    pub fn reboot(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("Reboot", interactive)
    }

    /// Requests rebooting directly into UEFI/BIOS firmware setup by calling
    /// `org.freedesktop.login1.Manager.SetRebootToFirmwareSetup(true)` followed by `Reboot`.
    ///
    /// If `SetRebootToFirmwareSetup` fails (for example on Legacy BIOS systems or denied Polkit
    /// authorization), the reboot is aborted cleanly rather than performing a normal reboot.
    pub fn reboot_to_firmware_setup(&self, interactive: bool) -> Result<()> {
        let timeout = if interactive {
            DBUS_SLOW_TIMEOUT
        } else {
            DBUS_FAST_TIMEOUT
        };
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Manager.RebootToFirmwareSetup",
                LOGIN1_MANAGER_PATH,
            )
            .with_dedup("logind:RebootToFirmwareSetup")
            .with_timeout(timeout),
            move |conn| async move {
                conn.call_method(
                    Some(LOGIN1_DEST),
                    LOGIN1_MANAGER_PATH,
                    Some(LOGIN1_MANAGER_IFACE),
                    "SetRebootToFirmwareSetup",
                    &(true,),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("Manager.SetRebootToFirmwareSetup failed: {e}"))
                })?;
                conn.call_method(
                    Some(LOGIN1_DEST),
                    LOGIN1_MANAGER_PATH,
                    Some(LOGIN1_MANAGER_IFACE),
                    "Reboot",
                    &(interactive,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Manager.Reboot failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Requests a userspace-only soft reboot (`systemd` >= 254) via
    /// `org.freedesktop.login1.Manager.SoftReboot`.
    pub fn soft_reboot(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("SoftReboot", interactive)
    }

    /// Requests system power-off (Shutdown) via `org.freedesktop.login1.Manager.PowerOff`.
    pub fn power_off(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("PowerOff", interactive)
    }

    /// Requests system halt (stops the OS without cutting hardware power) via
    /// `org.freedesktop.login1.Manager.Halt`.
    pub fn halt(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("Halt", interactive)
    }

    /// Acquires a sleep/idle/handle-power-key inhibitor lock from `systemd-logind`.
    ///
    /// Example `what`: `"sleep:idle"`, `mode`: `"block"` or `"delay"`.
    pub fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> Result<LogindInhibitor> {
        let conn = self.connection(DbusBus::System)?;
        let reply = conn
            .call_method(
                Some(LOGIN1_DEST),
                LOGIN1_MANAGER_PATH,
                Some(LOGIN1_MANAGER_IFACE),
                "Inhibit",
                &(what, who, why, mode),
            )
            .map_err(|e| WaywiError::DbusError(format!("Manager.Inhibit failed: {e}")))?;
        let fd: OwnedFd = reply
            .body()
            .deserialize()
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        Ok(LogindInhibitor { _fd: fd })
    }

    fn call_manager_bool(&self, method: &'static str, interactive: bool) -> Result<()> {
        let timeout = if interactive {
            DBUS_SLOW_TIMEOUT
        } else {
            DBUS_FAST_TIMEOUT
        };
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                format!("Manager.{method}"),
                LOGIN1_MANAGER_PATH,
            )
            .with_dedup(format!("logind:{method}"))
            .with_timeout(timeout),
            move |conn| async move {
                conn.call_method(
                    Some(LOGIN1_DEST),
                    LOGIN1_MANAGER_PATH,
                    Some(LOGIN1_MANAGER_IFACE),
                    method,
                    &(interactive,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Manager.{method} failed: {e}")))?;
                Ok(())
            },
        )
    }
}

pub(crate) struct LogindWorker {
    stop: AsyncStopSignal,
    join: Option<std::thread::JoinHandle<()>>,
}

impl LogindWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<LogindEvent>,
        shared: DbusSharedBus,
    ) -> Self {
        let stop = AsyncStopSignal::new();
        let stop_sig = stop.clone();
        let join = std::thread::Builder::new()
            .name("waywi-dbus-logind".to_owned())
            .spawn(move || {
                let Ok(conn) = connect_shared_bus(DbusBus::System, &shared) else {
                    return;
                };
                futures_lite::future::block_on(async move {
                    let mut cached_caps = read_power_capabilities_async(&conn).await;
                    let mut last_snap = read_logind_snapshot_async(&conn, cached_caps).await;
                    if let Some(ref s) = last_snap {
                        let _ = sender.send(LogindEvent::Snapshot(s.clone()));
                    }

                    let Ok(rule) = zbus::MatchRule::builder()
                        .msg_type(zbus::message::Type::Signal)
                        .sender(LOGIN1_DEST)
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
                        let member = msg.header().member().map(|m| m.as_str().to_owned());
                        match member.as_deref() {
                            Some("PreparingForSleep") => {
                                if let Ok(v) = msg.body().deserialize::<bool>() {
                                    let _ = sender.send(LogindEvent::PreparingForSleep(v));
                                }
                            }
                            Some("PreparingForShutdown") => {
                                if let Ok(v) = msg.body().deserialize::<bool>() {
                                    let _ = sender.send(LogindEvent::PreparingForShutdown(v));
                                }
                            }
                            Some("Lock") => {
                                let _ = sender.send(LogindEvent::LockRequested);
                            }
                            Some("Unlock") => {
                                let _ = sender.send(LogindEvent::UnlockRequested);
                            }
                            _ => {}
                        }
                        if let Some(mut snap) = read_logind_snapshot_async(&conn, cached_caps).await
                        {
                            let inhibitors_changed = last_snap
                                .as_ref()
                                .is_none_or(|prev| prev.block_inhibited != snap.block_inhibited);
                            if inhibitors_changed {
                                cached_caps = read_power_capabilities_async(&conn).await;
                                snap.power_capabilities = cached_caps;
                            }
                            if last_snap.as_ref() != Some(&snap) {
                                last_snap = Some(snap.clone());
                                let _ = sender.send(LogindEvent::Snapshot(snap));
                            }
                        }
                    }
                });
            })
            .ok();
        Self { stop, join }
    }
}

impl Drop for LogindWorker {
    fn drop(&mut self) {
        self.stop.trigger();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn resolve_session_path_blocking(conn: &zbus::blocking::Connection) -> Result<String> {
    if let Ok(sid) = env::var("XDG_SESSION_ID")
        && !sid.trim().is_empty()
        && let Ok(reply) = conn.call_method(
            Some(LOGIN1_DEST),
            LOGIN1_MANAGER_PATH,
            Some(LOGIN1_MANAGER_IFACE),
            "GetSession",
            &(sid.trim(),),
        )
        && let Ok(p) = reply.body().deserialize::<OwnedObjectPath>()
    {
        return Ok(p.to_string());
    }
    let reply = conn
        .call_method(
            Some(LOGIN1_DEST),
            LOGIN1_MANAGER_PATH,
            Some(LOGIN1_MANAGER_IFACE),
            "GetSessionByPID",
            &(std::process::id(),),
        )
        .map_err(|e| WaywiError::DbusError(format!("Manager.GetSessionByPID failed: {e}")))?;
    let p: OwnedObjectPath = reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
    Ok(p.to_string())
}

async fn resolve_session_path_async(conn: &zbus::Connection) -> Option<String> {
    if let Ok(sid) = env::var("XDG_SESSION_ID")
        && !sid.trim().is_empty()
        && let Ok(reply) = conn
            .call_method(
                Some(LOGIN1_DEST),
                LOGIN1_MANAGER_PATH,
                Some(LOGIN1_MANAGER_IFACE),
                "GetSession",
                &(sid.trim(),),
            )
            .await
        && let Ok(p) = reply.body().deserialize::<OwnedObjectPath>()
    {
        return Some(p.to_string());
    }
    let reply = conn
        .call_method(
            Some(LOGIN1_DEST),
            LOGIN1_MANAGER_PATH,
            Some(LOGIN1_MANAGER_IFACE),
            "GetSessionByPID",
            &(std::process::id(),),
        )
        .await
        .ok()?;
    reply
        .body()
        .deserialize::<OwnedObjectPath>()
        .ok()
        .map(|p| p.to_string())
}

fn query_can_blocking(
    conn: &zbus::blocking::Connection,
    method: &'static str,
) -> PowerActionSupport {
    let Ok(reply) = conn.call_method(
        Some(LOGIN1_DEST),
        LOGIN1_MANAGER_PATH,
        Some(LOGIN1_MANAGER_IFACE),
        method,
        &(),
    ) else {
        return PowerActionSupport::NotAvailable;
    };
    reply
        .body()
        .deserialize::<String>()
        .map(|s| PowerActionSupport::from_logind_str(&s))
        .unwrap_or(PowerActionSupport::NotAvailable)
}

fn read_power_capabilities_blocking(conn: &zbus::blocking::Connection) -> LogindPowerCapabilities {
    LogindPowerCapabilities {
        power_off: query_can_blocking(conn, "CanPowerOff"),
        reboot: query_can_blocking(conn, "CanReboot"),
        reboot_to_firmware_setup: query_can_blocking(conn, "CanRebootToFirmwareSetup"),
        suspend: query_can_blocking(conn, "CanSuspend"),
        hibernate: query_can_blocking(conn, "CanHibernate"),
        hybrid_sleep: query_can_blocking(conn, "CanHybridSleep"),
        suspend_then_hibernate: query_can_blocking(conn, "CanSuspendThenHibernate"),
        soft_reboot: query_can_blocking(conn, "CanSoftReboot"),
        halt: query_can_blocking(conn, "CanHalt"),
    }
}

async fn query_can_async(conn: &zbus::Connection, method: &'static str) -> PowerActionSupport {
    let Ok(reply) = conn
        .call_method(
            Some(LOGIN1_DEST),
            LOGIN1_MANAGER_PATH,
            Some(LOGIN1_MANAGER_IFACE),
            method,
            &(),
        )
        .await
    else {
        return PowerActionSupport::NotAvailable;
    };
    reply
        .body()
        .deserialize::<String>()
        .map(|s| PowerActionSupport::from_logind_str(&s))
        .unwrap_or(PowerActionSupport::NotAvailable)
}

async fn read_power_capabilities_async(conn: &zbus::Connection) -> LogindPowerCapabilities {
    use futures_lite::future::zip;
    let (
        ((power_off, reboot), (reboot_to_firmware_setup, suspend)),
        ((hibernate, hybrid_sleep), ((suspend_then_hibernate, soft_reboot), halt)),
    ) = zip(
        zip(
            zip(
                query_can_async(conn, "CanPowerOff"),
                query_can_async(conn, "CanReboot"),
            ),
            zip(
                query_can_async(conn, "CanRebootToFirmwareSetup"),
                query_can_async(conn, "CanSuspend"),
            ),
        ),
        zip(
            zip(
                query_can_async(conn, "CanHibernate"),
                query_can_async(conn, "CanHybridSleep"),
            ),
            zip(
                zip(
                    query_can_async(conn, "CanSuspendThenHibernate"),
                    query_can_async(conn, "CanSoftReboot"),
                ),
                query_can_async(conn, "CanHalt"),
            ),
        ),
    )
    .await;
    LogindPowerCapabilities {
        power_off,
        reboot,
        reboot_to_firmware_setup,
        suspend,
        hibernate,
        hybrid_sleep,
        suspend_then_hibernate,
        soft_reboot,
        halt,
    }
}

fn read_logind_snapshot_blocking(conn: &zbus::blocking::Connection) -> Result<LogindSnapshot> {
    let mgr = get_all_blocking(conn, LOGIN1_DEST, LOGIN1_MANAGER_PATH, LOGIN1_MANAGER_IFACE)?;
    let session_path =
        resolve_session_path_blocking(conn).unwrap_or_else(|_| LOGIN1_SESSION_AUTO.to_owned());
    let sess = get_all_blocking(conn, LOGIN1_DEST, &session_path, LOGIN1_SESSION_IFACE)
        .unwrap_or_default();
    let power_capabilities = read_power_capabilities_blocking(conn);
    Ok(assemble_logind_snapshot(
        session_path,
        &mgr,
        &sess,
        power_capabilities,
    ))
}

async fn read_logind_snapshot_async(
    conn: &zbus::Connection,
    power_capabilities: LogindPowerCapabilities,
) -> Option<LogindSnapshot> {
    let mgr = get_all_async(conn, LOGIN1_DEST, LOGIN1_MANAGER_PATH, LOGIN1_MANAGER_IFACE).await?;
    let session_path = resolve_session_path_async(conn)
        .await
        .unwrap_or_else(|| LOGIN1_SESSION_AUTO.to_owned());
    let sess = get_all_async(conn, LOGIN1_DEST, &session_path, LOGIN1_SESSION_IFACE)
        .await
        .unwrap_or_default();
    Some(assemble_logind_snapshot(
        session_path,
        &mgr,
        &sess,
        power_capabilities,
    ))
}

pub(crate) fn assemble_logind_snapshot(
    session_path: String,
    mgr: &HashMap<String, OwnedValue>,
    sess: &HashMap<String, OwnedValue>,
    power_capabilities: LogindPowerCapabilities,
) -> LogindSnapshot {
    LogindSnapshot {
        session_id: dict_str(sess, "Id").unwrap_or_default(),
        session_path,
        locked_hint: dict_bool(sess, "LockedHint").unwrap_or(false),
        idle_hint: dict_bool(sess, "IdleHint").unwrap_or(false),
        idle_since_monotonic_us: dict_u64(sess, "IdleSinceHintMonotonic").unwrap_or(0),
        preparing_for_sleep: dict_bool(mgr, "PreparingForSleep").unwrap_or(false),
        preparing_for_shutdown: dict_bool(mgr, "PreparingForShutdown").unwrap_or(false),
        block_inhibited: dict_str(mgr, "BlockInhibited").unwrap_or_default(),
        power_capabilities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn assembles_logind_snapshot_from_manager_and_session_dicts() {
        let mut mgr = HashMap::new();
        mgr.insert(
            "PreparingForSleep".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        mgr.insert(
            "BlockInhibited".into(),
            OwnedValue::try_from(Value::from("sleep:idle")).unwrap(),
        );
        let mut sess = HashMap::new();
        sess.insert(
            "Id".into(),
            OwnedValue::try_from(Value::from("42")).unwrap(),
        );
        sess.insert(
            "LockedHint".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        sess.insert(
            "IdleHint".into(),
            OwnedValue::try_from(Value::from(false)).unwrap(),
        );

        let caps = LogindPowerCapabilities {
            power_off: PowerActionSupport::Yes,
            reboot: PowerActionSupport::Challenge,
            hibernate: PowerActionSupport::NotAvailable,
            ..LogindPowerCapabilities::default()
        };
        let snap = assemble_logind_snapshot(
            "/org/freedesktop/login1/session/_342".into(),
            &mgr,
            &sess,
            caps,
        );
        assert_eq!(snap.session_id, "42");
        assert!(snap.locked_hint);
        assert!(snap.preparing_for_sleep);
        assert_eq!(snap.block_inhibited, "sleep:idle");
        assert_eq!(snap.power_capabilities.power_off, PowerActionSupport::Yes);
        assert_eq!(
            snap.power_capabilities.reboot,
            PowerActionSupport::Challenge
        );
        assert_eq!(
            snap.power_capabilities.hibernate,
            PowerActionSupport::NotAvailable
        );
    }

    #[test]
    fn parses_logind_power_action_support_strings_and_helpers() {
        assert_eq!(
            PowerActionSupport::from_logind_str("yes"),
            PowerActionSupport::Yes
        );
        assert!(PowerActionSupport::Yes.is_supported());
        assert!(!PowerActionSupport::Yes.needs_auth());

        assert_eq!(
            PowerActionSupport::from_logind_str("challenge"),
            PowerActionSupport::Challenge
        );
        assert!(PowerActionSupport::Challenge.is_supported());
        assert!(PowerActionSupport::Challenge.needs_auth());

        assert_eq!(
            PowerActionSupport::from_logind_str("no"),
            PowerActionSupport::No
        );
        assert!(!PowerActionSupport::No.is_supported());

        assert_eq!(
            PowerActionSupport::from_logind_str("na"),
            PowerActionSupport::NotAvailable
        );
        assert_eq!(
            PowerActionSupport::from_logind_str(""),
            PowerActionSupport::NotAvailable
        );
        assert!(!PowerActionSupport::NotAvailable.is_supported());
    }
}
