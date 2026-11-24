//! systemd-logind session lock/idle state, backlight brightness, inhibitors, and power actions
//! (`org.freedesktop.login1`).

use std::collections::HashMap;
use std::env;

use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{
    DBUS_FAST_TIMEOUT, DBUS_SLOW_TIMEOUT, DbusActionLane, DbusClient, DbusSharedBus,
    LaneActionSpec, connect_shared_bus, validate_object_path,
};
use super::types::{Dbus, DbusBus};
use super::worker::DbusWorkerHandle;
use super::zvariant_helpers::{dict_bool, dict_str, dict_u64, get_all};

const LOGIN1_DEST: &str = "org.freedesktop.login1";
const LOGIN1_MANAGER_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";
const LOGIN1_SESSION_IFACE: &str = "org.freedesktop.login1.Session";
const LOGIN1_SESSION_AUTO: &str = "/org/freedesktop/login1/session/auto";

/// System power transition action executed via [`LogindClient::power`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerAction {
    Suspend,
    Hibernate,
    HybridSleep,
    SuspendThenHibernate,
    Reboot,
    RebootToFirmwareSetup,
    SoftReboot,
    PowerOff,
    Halt,
}

impl PowerAction {
    const fn method_name(self) -> &'static str {
        match self {
            Self::Suspend => "Suspend",
            Self::Hibernate => "Hibernate",
            Self::HybridSleep => "HybridSleep",
            Self::SuspendThenHibernate => "SuspendThenHibernate",
            Self::Reboot => "Reboot",
            Self::RebootToFirmwareSetup => "RebootToFirmwareSetup",
            Self::SoftReboot => "SoftReboot",
            Self::PowerOff => "PowerOff",
            Self::Halt => "Halt",
        }
    }
}

/// RAII sleep/idle/shutdown inhibitor guard returned by [`LogindClient::inhibit_blocking`].
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
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for `systemd-logind` session, brightness, inhibitor, and power actions.
#[derive(Clone, Copy)]
pub struct LogindClient<'a>(pub(super) &'a DbusClient);

impl LogindClient<'_> {
    /// Returns the latest cached [`LogindSnapshot`] from an active [`Dbus::logind`] subscription
    /// without issuing any D-Bus calls (0 ms).
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<LogindSnapshot> {
        self.0.cache.logind.read().ok().and_then(|g| g.clone())
    }

    /// Queries the system bus synchronously for the active user session [`LogindSnapshot`].
    pub fn snapshot_blocking(&self) -> Result<LogindSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let caps = read_power_capabilities(&conn).await;
            read_logind_snapshot(&conn, caps).await
        })
    }

    /// Queries the system bus synchronously for real-time [`LogindPowerCapabilities`].
    pub fn power_capabilities_blocking(&self) -> Result<LogindPowerCapabilities> {
        let conn = self.0.async_connection(DbusBus::System)?;
        Ok(futures_lite::future::block_on(read_power_capabilities(
            &conn,
        )))
    }

    /// Executes a system power transition (`Suspend`, `Hibernate`, `Reboot`, `PowerOff`,
    /// `RebootToFirmwareSetup`, etc.) via `org.freedesktop.login1.Manager`.
    ///
    /// For [`PowerAction::RebootToFirmwareSetup`], calls `SetRebootToFirmwareSetup(true)` before
    /// `Reboot(interactive)` and aborts cleanly if firmware setup is unsupported or denied.
    pub fn power(&self, action: PowerAction, interactive: bool) -> Result<()> {
        let method = action.method_name();
        let timeout = if interactive {
            DBUS_SLOW_TIMEOUT
        } else {
            DBUS_FAST_TIMEOUT
        };
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                format!("Manager.{method}"),
                LOGIN1_MANAGER_PATH,
            )
            .with_dedup(format!("logind:{method}"))
            .with_timeout(timeout),
            move |conn| async move {
                if matches!(action, PowerAction::RebootToFirmwareSetup) {
                    conn.call_method(
                        Some(LOGIN1_DEST),
                        LOGIN1_MANAGER_PATH,
                        Some(LOGIN1_MANAGER_IFACE),
                        "SetRebootToFirmwareSetup",
                        &(true,),
                    )
                    .await
                    .map_err(|e| {
                        WaywiError::DbusError(format!(
                            "Manager.SetRebootToFirmwareSetup failed: {e}"
                        ))
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
                } else {
                    conn.call_method(
                        Some(LOGIN1_DEST),
                        LOGIN1_MANAGER_PATH,
                        Some(LOGIN1_MANAGER_IFACE),
                        method,
                        &(interactive,),
                    )
                    .await
                    .map_err(|e| WaywiError::DbusError(format!("Manager.{method} failed: {e}")))?;
                }
                Ok(())
            },
        )
    }

    /// Requests locking a logind session (`None` targets the active session).
    pub fn lock_session(&self, session_id: Option<&str>) -> Result<()> {
        self.call_session_action(session_id, "Lock", "lock")
    }

    /// Requests unlocking a logind session (`None` targets the active session).
    pub fn unlock_session(&self, session_id: Option<&str>) -> Result<()> {
        self.call_session_action(session_id, "Unlock", "unlock")
    }

    /// Requests terminating (logging out) a logind session (`None` targets the active session).
    pub fn terminate_session(&self, session_id: Option<&str>) -> Result<()> {
        self.call_session_action(session_id, "Terminate", "terminate")
    }

    /// Sets device brightness via `org.freedesktop.login1.Session.SetBrightness` without root.
    pub fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> Result<()> {
        let sub = subsystem.to_owned();
        let dev = name.to_owned();
        let target = format!("{sub}:{dev}");
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Session.SetBrightness",
                target,
            ),
            move |conn| async move {
                let session_path = resolve_session_path(&conn, None)
                    .await
                    .unwrap_or_else(|_| LOGIN1_SESSION_AUTO.to_owned());
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

    /// Acquires a sleep/idle/handle-power-key inhibitor lock synchronously from `systemd-logind`.
    ///
    /// Example `what`: `"sleep:idle"`, `mode`: `"block"` or `"delay"`.
    pub fn inhibit_blocking(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> Result<LogindInhibitor> {
        let conn = self.0.async_connection(DbusBus::System)?;
        let what = what.to_owned();
        let who = who.to_owned();
        let why = why.to_owned();
        let mode = mode.to_owned();
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(LOGIN1_DEST),
                    LOGIN1_MANAGER_PATH,
                    Some(LOGIN1_MANAGER_IFACE),
                    "Inhibit",
                    &(what.as_str(), who.as_str(), why.as_str(), mode.as_str()),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Manager.Inhibit failed: {e}")))?;
            let fd: OwnedFd = reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            Ok(LogindInhibitor { _fd: fd })
        })
    }

    fn call_session_action(
        &self,
        session_id: Option<&str>,
        method: &'static str,
        dedup_tag: &'static str,
    ) -> Result<()> {
        let explicit = match session_id {
            Some(sid) => {
                let trimmed = sid.trim();
                if trimmed.is_empty() {
                    return Err(WaywiError::DbusError("empty logind session_id".into()));
                }
                if trimmed.starts_with('/') {
                    Some(validate_object_path(trimmed, "logind session path")?)
                } else {
                    Some(trimmed.to_owned())
                }
            }
            None => None,
        };
        let target = explicit
            .clone()
            .unwrap_or_else(|| LOGIN1_SESSION_AUTO.to_owned());
        let dedup = format!("logind:{dedup_tag}:{target}");
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                format!("Session.{method}"),
                target,
            )
            .with_dedup(dedup),
            move |conn| async move {
                let path = match explicit.as_deref() {
                    Some(sid) => resolve_session_path(&conn, Some(sid)).await?,
                    None => resolve_session_path(&conn, None)
                        .await
                        .unwrap_or_else(|_| LOGIN1_SESSION_AUTO.to_owned()),
                };
                conn.call_method(
                    Some(LOGIN1_DEST),
                    path.as_str(),
                    Some(LOGIN1_SESSION_IFACE),
                    method,
                    &(),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Session.{method} failed: {e}")))?;
                Ok(())
            },
        )
    }
}

pub(crate) struct LogindWorker(#[allow(dead_code)] DbusWorkerHandle);

impl LogindWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<LogindEvent>,
        shared: DbusSharedBus,
    ) -> Self {
        Self(DbusWorkerHandle::spawn(
            "waywi-dbus-logind",
            move |stop_sig| {
                let Ok(conn) = connect_shared_bus(DbusBus::System, &shared) else {
                    return;
                };
                futures_lite::future::block_on(async move {
                    let mut cached_caps = read_power_capabilities(&conn).await;
                    let mut last_snap = read_logind_snapshot(&conn, cached_caps).await.ok();
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
                        dispatch_logind_signal(&msg, &sender);

                        // Process any additional queued signals immediately so we preserve every
                        // discrete signal while refreshing the snapshot once per burst.
                        let mut stream_closed = false;
                        while let Some(item) = futures_lite::future::poll_once(stream.next()).await
                        {
                            match item {
                                Some(Ok(extra)) => dispatch_logind_signal(&extra, &sender),
                                Some(Err(_)) | None => {
                                    stream_closed = true;
                                    break;
                                }
                            }
                        }

                        if let Ok(mut snap) = read_logind_snapshot(&conn, cached_caps).await {
                            let inhibitors_changed = last_snap
                                .as_ref()
                                .is_none_or(|prev| prev.block_inhibited != snap.block_inhibited);
                            if inhibitors_changed {
                                cached_caps = read_power_capabilities(&conn).await;
                                snap.power_capabilities = cached_caps;
                            }
                            if last_snap.as_ref() != Some(&snap) {
                                last_snap = Some(snap.clone());
                                let _ = sender.send(LogindEvent::Snapshot(snap));
                            }
                        }
                        if stream_closed {
                            break;
                        }
                    }
                });
            },
        ))
    }
}

fn dispatch_logind_signal(msg: &zbus::Message, sender: &calloop::channel::Sender<LogindEvent>) {
    match msg.header().member().map(|m| m.as_str()) {
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
}

async fn resolve_session_path(
    conn: &zbus::Connection,
    explicit_session_id: Option<&str>,
) -> Result<String> {
    if let Some(sid) = explicit_session_id {
        if sid.starts_with('/') {
            return Ok(sid.to_owned());
        }
        let reply = conn
            .call_method(
                Some(LOGIN1_DEST),
                LOGIN1_MANAGER_PATH,
                Some(LOGIN1_MANAGER_IFACE),
                "GetSession",
                &(sid,),
            )
            .await
            .map_err(|e| WaywiError::DbusError(format!("Manager.GetSession({sid}) failed: {e}")))?;
        let p: OwnedObjectPath = reply
            .body()
            .deserialize()
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        return Ok(p.to_string());
    }

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
        .await
        .map_err(|e| WaywiError::DbusError(format!("Manager.GetSessionByPID failed: {e}")))?;
    let p: OwnedObjectPath = reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
    Ok(p.to_string())
}

async fn query_can(conn: &zbus::Connection, method: &'static str) -> PowerActionSupport {
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

async fn read_power_capabilities(conn: &zbus::Connection) -> LogindPowerCapabilities {
    use futures_lite::future::zip;
    let (
        ((power_off, reboot), (reboot_to_firmware_setup, suspend)),
        ((hibernate, hybrid_sleep), ((suspend_then_hibernate, soft_reboot), halt)),
    ) = zip(
        zip(
            zip(query_can(conn, "CanPowerOff"), query_can(conn, "CanReboot")),
            zip(
                query_can(conn, "CanRebootToFirmwareSetup"),
                query_can(conn, "CanSuspend"),
            ),
        ),
        zip(
            zip(
                query_can(conn, "CanHibernate"),
                query_can(conn, "CanHybridSleep"),
            ),
            zip(
                zip(
                    query_can(conn, "CanSuspendThenHibernate"),
                    query_can(conn, "CanSoftReboot"),
                ),
                query_can(conn, "CanHalt"),
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

async fn read_logind_snapshot(
    conn: &zbus::Connection,
    power_capabilities: LogindPowerCapabilities,
) -> Result<LogindSnapshot> {
    let mgr = get_all(conn, LOGIN1_DEST, LOGIN1_MANAGER_PATH, LOGIN1_MANAGER_IFACE).await?;
    let session_path = resolve_session_path(conn, None)
        .await
        .unwrap_or_else(|_| LOGIN1_SESSION_AUTO.to_owned());
    let sess = get_all(conn, LOGIN1_DEST, &session_path, LOGIN1_SESSION_IFACE)
        .await
        .unwrap_or_default();
    Ok(assemble_logind_snapshot(
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
        assert_eq!(
            PowerAction::RebootToFirmwareSetup.method_name(),
            "RebootToFirmwareSetup"
        );
    }
}
