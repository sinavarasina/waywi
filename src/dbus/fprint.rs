//! Fingerprint reader integration (`net.reactivated.Fprint.Manager` & `Device`).
//!
//! Supports listing readers/enrolled fingers and running a zero-polling fingerprint verification
//! stream (`Dbus::fprint_verify`) that automatically claims the default device, starts
//! verification, and stops/releases the device cleanly when unsubscribed.

use futures_lite::StreamExt;
use zbus::zvariant::OwnedObjectPath;

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{DbusClient, DbusSharedBus, connect_shared_bus, validate_object_path};
use super::types::{Dbus, DbusBus};
use super::worker::DbusWorkerHandle;
use super::zvariant_helpers::{dict_i32, dict_str, get_all};

const FPRINT_DEST: &str = "net.reactivated.Fprint";
const FPRINT_MGR_PATH: &str = "/net/reactivated/Fprint/Manager";
const FPRINT_MGR_IFACE: &str = "net.reactivated.Fprint.Manager";
const FPRINT_DEV_IFACE: &str = "net.reactivated.Fprint.Device";

/// Metadata for a fingerprint reader exposed by `net.reactivated.Fprint.Device`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FprintDeviceInfo {
    pub object_path: String,
    pub name: String,
    pub num_enroll_stages: i32,
    pub scan_type: String,
}

/// Decoded verification status from `net.reactivated.Fprint.Device.VerifyStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FprintVerifyResult {
    Match,
    NoMatch,
    RetryScan,
    SwipeTooShort,
    FingerNotCentered,
    RemoveAndRetry,
    Disconnected,
    UnknownError,
    Other(String),
}

impl FprintVerifyResult {
    #[must_use]
    pub fn from_fprint_str(raw: &str) -> Self {
        match raw {
            "verify-match" => Self::Match,
            "verify-no-match" => Self::NoMatch,
            "verify-retry-scan" => Self::RetryScan,
            "verify-swipe-too-short" => Self::SwipeTooShort,
            "verify-finger-not-centered" => Self::FingerNotCentered,
            "verify-remove-and-retry" => Self::RemoveAndRetry,
            "verify-disconnected" => Self::Disconnected,
            "verify-unknown-error" => Self::UnknownError,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// Events emitted by [`Dbus::fprint_verify`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FprintEvent {
    Ready {
        device_path: String,
    },
    FingerSelected {
        finger: String,
    },
    VerifyStatus {
        result: FprintVerifyResult,
        done: bool,
    },
    Error(String),
}

impl Dbus {
    /// Claims the default `fprintd` device for `username` (pass `""` for the calling user),
    /// starts `VerifyStart("any")`, streams [`FprintEvent`]s zero-polling, and stops/releases the
    /// device automatically on drop.
    pub fn fprint_verify<M: 'static>(
        username: impl Into<String>,
        map: impl Fn(FprintEvent) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusFprintVerify {
                username: username.into(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for `net.reactivated.Fprint` queries.
#[derive(Clone, Copy)]
pub struct FprintClient<'a>(pub(super) &'a DbusClient);

impl FprintClient<'_> {
    /// Lists available fingerprint reader devices synchronously.
    pub fn devices_blocking(&self) -> Result<Vec<FprintDeviceInfo>> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(FPRINT_DEST),
                    FPRINT_MGR_PATH,
                    Some(FPRINT_MGR_IFACE),
                    "GetDevices",
                    &(),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Fprint.GetDevices failed: {e}")))?;
            let paths: Vec<OwnedObjectPath> = reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            let mut out = Vec::new();
            for path in paths.into_iter().take(16) {
                if let Ok(props) =
                    get_all(&conn, FPRINT_DEST, path.as_str(), FPRINT_DEV_IFACE).await
                {
                    out.push(FprintDeviceInfo {
                        object_path: path.to_string(),
                        name: dict_str(&props, "name").unwrap_or_default(),
                        num_enroll_stages: dict_i32(&props, "num-enroll-stages").unwrap_or(0),
                        scan_type: dict_str(&props, "scan-type").unwrap_or_default(),
                    });
                }
            }
            Ok(out)
        })
    }

    /// Lists enrolled finger names for `username` on `device_path` synchronously.
    pub fn list_enrolled_fingers_blocking(
        &self,
        device_path: &str,
        username: &str,
    ) -> Result<Vec<String>> {
        let path = validate_object_path(device_path, "fprint device path")?;
        let user = username.to_owned();
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(FPRINT_DEST),
                    path.as_str(),
                    Some(FPRINT_DEV_IFACE),
                    "ListEnrolledFingers",
                    &(user.as_str(),),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("Fprint.ListEnrolledFingers failed: {e}"))
                })?;
            reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))
        })
    }
}

pub(crate) struct FprintVerifyWorker(#[allow(dead_code)] DbusWorkerHandle);

impl FprintVerifyWorker {
    pub(crate) fn spawn(
        username: String,
        sender: calloop::channel::Sender<FprintEvent>,
        shared: DbusSharedBus,
    ) -> Self {
        Self(DbusWorkerHandle::spawn("waywi-dbus-fprint", move |stop| {
            let Ok(conn) = connect_shared_bus(DbusBus::System, &shared) else {
                let _ = sender.send(FprintEvent::Error("system D-Bus unavailable".into()));
                return;
            };
            futures_lite::future::block_on(async move {
                let dev_reply = match conn
                    .call_method(
                        Some(FPRINT_DEST),
                        FPRINT_MGR_PATH,
                        Some(FPRINT_MGR_IFACE),
                        "GetDefaultDevice",
                        &(),
                    )
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = sender.send(FprintEvent::Error(format!(
                            "Fprint.GetDefaultDevice failed: {e}"
                        )));
                        return;
                    }
                };
                let Ok(dev_path) = dev_reply.body().deserialize::<OwnedObjectPath>() else {
                    return;
                };

                let rule = match zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender(FPRINT_DEST)
                    .and_then(|b| b.interface(FPRINT_DEV_IFACE))
                    .and_then(|b| b.path(dev_path.as_str()))
                {
                    Ok(r) => r.build(),
                    Err(_) => return,
                };
                let Ok(mut stream) =
                    zbus::MessageStream::for_match_rule(rule, &conn, Some(16)).await
                else {
                    return;
                };

                if let Err(e) = conn
                    .call_method(
                        Some(FPRINT_DEST),
                        dev_path.as_str(),
                        Some(FPRINT_DEV_IFACE),
                        "Claim",
                        &(username.as_str(),),
                    )
                    .await
                {
                    let _ = sender.send(FprintEvent::Error(format!("Fprint.Claim failed: {e}")));
                    return;
                }

                if let Err(e) = conn
                    .call_method(
                        Some(FPRINT_DEST),
                        dev_path.as_str(),
                        Some(FPRINT_DEV_IFACE),
                        "VerifyStart",
                        &("any",),
                    )
                    .await
                {
                    let _ = conn
                        .call_method(
                            Some(FPRINT_DEST),
                            dev_path.as_str(),
                            Some(FPRINT_DEV_IFACE),
                            "Release",
                            &(),
                        )
                        .await;
                    let _ = sender.send(FprintEvent::Error(format!(
                        "Fprint.VerifyStart failed: {e}"
                    )));
                    return;
                }

                let _ = sender.send(FprintEvent::Ready {
                    device_path: dev_path.to_string(),
                });

                while !stop.is_stopped() {
                    let next =
                        futures_lite::future::or(async { Some(stream.next().await) }, async {
                            stop.wait_stopped().await;
                            None
                        })
                        .await;
                    let Some(Some(Ok(msg))) = next else {
                        break;
                    };
                    let member = msg.header().member().map(|m| m.as_str().to_owned());
                    match member.as_deref() {
                        Some("VerifyFingerSelected") => {
                            if let Ok(finger) = msg.body().deserialize::<String>() {
                                let _ = sender.send(FprintEvent::FingerSelected { finger });
                            }
                        }
                        Some("VerifyStatus") => {
                            if let Ok((res_str, done)) = msg.body().deserialize::<(String, bool)>()
                            {
                                let _ = sender.send(FprintEvent::VerifyStatus {
                                    result: FprintVerifyResult::from_fprint_str(&res_str),
                                    done,
                                });
                                if done {
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let _ = conn
                    .call_method(
                        Some(FPRINT_DEST),
                        dev_path.as_str(),
                        Some(FPRINT_DEV_IFACE),
                        "VerifyStop",
                        &(),
                    )
                    .await;
                let _ = conn
                    .call_method(
                        Some(FPRINT_DEST),
                        dev_path.as_str(),
                        Some(FPRINT_DEV_IFACE),
                        "Release",
                        &(),
                    )
                    .await;
            });
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_fprint_verify_status_strings() {
        assert_eq!(
            FprintVerifyResult::from_fprint_str("verify-match"),
            FprintVerifyResult::Match
        );
        assert_eq!(
            FprintVerifyResult::from_fprint_str("verify-no-match"),
            FprintVerifyResult::NoMatch
        );
        assert_eq!(
            FprintVerifyResult::from_fprint_str("verify-finger-not-centered"),
            FprintVerifyResult::FingerNotCentered
        );
        assert_eq!(
            FprintVerifyResult::from_fprint_str("verify-custom"),
            FprintVerifyResult::Other("verify-custom".into())
        );
    }
}
