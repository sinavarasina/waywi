//! systemd-logind session lock/idle state, backlight brightness, inhibitors, and power actions
//! (`org.freedesktop.login1`).

use std::collections::HashMap;
use std::env;

use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{
    DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec, connect_shared_bus,
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

    /// Sets device brightness via `org.freedesktop.login1.Session.SetBrightness` without root or
    /// blocking the UI thread.
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

    /// Requests locking the active session via `org.freedesktop.login1.Session.Lock` without
    /// blocking the UI thread.
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

    /// Requests unlocking the active session via `org.freedesktop.login1.Session.Unlock` without
    /// blocking the UI thread.
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

    /// Requests system suspend via `org.freedesktop.login1.Manager.Suspend`.
    pub fn suspend(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("Suspend", interactive)
    }

    /// Requests system hibernate via `org.freedesktop.login1.Manager.Hibernate`.
    pub fn hibernate(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("Hibernate", interactive)
    }

    /// Requests system reboot via `org.freedesktop.login1.Manager.Reboot`.
    pub fn reboot(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("Reboot", interactive)
    }

    /// Requests system power-off via `org.freedesktop.login1.Manager.PowerOff`.
    pub fn power_off(&self, interactive: bool) -> Result<()> {
        self.call_manager_bool("PowerOff", interactive)
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
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                format!("Manager.{method}"),
                LOGIN1_MANAGER_PATH,
            ),
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
                    let mut last_snap = read_logind_snapshot_async(&conn).await;
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
                        if let Some(snap) = read_logind_snapshot_async(&conn).await
                            && last_snap.as_ref() != Some(&snap)
                        {
                            last_snap = Some(snap.clone());
                            let _ = sender.send(LogindEvent::Snapshot(snap));
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

fn read_logind_snapshot_blocking(conn: &zbus::blocking::Connection) -> Result<LogindSnapshot> {
    let mgr = get_all_blocking(conn, LOGIN1_DEST, LOGIN1_MANAGER_PATH, LOGIN1_MANAGER_IFACE)?;
    let session_path =
        resolve_session_path_blocking(conn).unwrap_or_else(|_| LOGIN1_SESSION_AUTO.to_owned());
    let sess = get_all_blocking(conn, LOGIN1_DEST, &session_path, LOGIN1_SESSION_IFACE)
        .unwrap_or_default();
    Ok(assemble_logind_snapshot(session_path, &mgr, &sess))
}

async fn read_logind_snapshot_async(conn: &zbus::Connection) -> Option<LogindSnapshot> {
    let mgr = get_all_async(conn, LOGIN1_DEST, LOGIN1_MANAGER_PATH, LOGIN1_MANAGER_IFACE).await?;
    let session_path = resolve_session_path_async(conn)
        .await
        .unwrap_or_else(|| LOGIN1_SESSION_AUTO.to_owned());
    let sess = get_all_async(conn, LOGIN1_DEST, &session_path, LOGIN1_SESSION_IFACE)
        .await
        .unwrap_or_default();
    Some(assemble_logind_snapshot(session_path, &mgr, &sess))
}

pub(crate) fn assemble_logind_snapshot(
    session_path: String,
    mgr: &HashMap<String, OwnedValue>,
    sess: &HashMap<String, OwnedValue>,
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

        let snap =
            assemble_logind_snapshot("/org/freedesktop/login1/session/_342".into(), &mgr, &sess);
        assert_eq!(snap.session_id, "42");
        assert!(snap.locked_hint);
        assert!(snap.preparing_for_sleep);
        assert_eq!(snap.block_inhibited, "sleep:idle");
    }
}
