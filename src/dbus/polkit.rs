//! Polkit authentication agent (`org.freedesktop.PolicyKit1.AuthenticationAgent`) and
//! authorization check client (`org.freedesktop.PolicyKit1.Authority`).
//!
//! Enables Wayland desktop shells built with WayWi to act as a native Polkit authentication
//! agent without external helper daemons, driving `polkit-agent-helper-1` on a background thread
//! when the user supplies credentials.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use zbus::zvariant::{OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::DbusClient;
use super::types::{Dbus, DbusBus};
use super::worker::{AsyncReplySlot, AsyncStopSignal, DbusWorkerHandle};

const POLKIT_DEST: &str = "org.freedesktop.PolicyKit1";
const POLKIT_AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const POLKIT_AUTHORITY_IFACE: &str = "org.freedesktop.PolicyKit1.Authority";
const POLKIT_AGENT_PATH: &str = "/org/waywi/PolicyKit1/AuthenticationAgent";

/// A Polkit identity selectable for authentication (typically a Unix user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolkitIdentity {
    pub kind: String,
    pub uid: Option<u32>,
    pub user_name: Option<String>,
}

/// Incoming authentication challenge from `org.freedesktop.PolicyKit1.Authority`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolkitAuthRequest {
    pub cookie: String,
    pub action_id: String,
    pub message: String,
    pub icon_name: String,
    pub details: HashMap<String, String>,
    pub identities: Vec<PolkitIdentity>,
}

/// Application reply to a pending [`PolkitAuthRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolkitAgentReply {
    /// Authenticate `user` with `response` (password/PIN) via `polkit-agent-helper-1`.
    Authenticate { user: String, response: String },
    /// Authentication was already completed externally (completes the D-Bus call cleanly).
    Complete,
    /// User cancelled or dismissed the prompt.
    Cancel,
}

/// Events emitted by [`Dbus::polkit_agent`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolkitAgentEvent {
    Ready,
    BeginAuthentication(Box<PolkitAuthRequest>),
    CancelAuthentication {
        cookie: String,
    },
    HelperPrompt {
        cookie: String,
        prompt: String,
        echo: bool,
    },
    HelperInfo {
        cookie: String,
        message: String,
    },
    HelperError {
        cookie: String,
        message: String,
    },
    Completed {
        cookie: String,
        authorized: bool,
    },
    Error(String),
}

/// Outcome of [`PolkitClient::check_authorization_blocking`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolkitAuthResult {
    pub is_authorized: bool,
    pub is_challenge: bool,
    pub details: HashMap<String, String>,
}

impl Dbus {
    /// Registers an `org.freedesktop.PolicyKit1.AuthenticationAgent` on the system bus for the
    /// current session and streams incoming authentication challenges.
    pub fn polkit_agent<M: 'static>(
        map: impl Fn(PolkitAgentEvent) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusPolkitAgent {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for Polkit agent responses and authorization checks.
#[derive(Clone, Copy)]
pub struct PolkitClient<'a>(pub(super) &'a DbusClient);

impl PolkitClient<'_> {
    /// Responds to a pending [`PolkitAuthRequest`] identified by `cookie`.
    pub fn reply(&self, cookie: &str, reply: PolkitAgentReply) -> Result<()> {
        let slot = self
            .0
            .polkit_agent_pending
            .lock()
            .map_err(|_| WaywiError::DbusError("polkit agent pending lock poisoned".into()))?
            .remove(cookie)
            .ok_or_else(|| {
                WaywiError::DbusError(format!("no pending Polkit request for cookie `{cookie}`"))
            })?;
        slot.resolve(reply);
        Ok(())
    }

    /// Checks whether the current process is authorized for `action_id` via
    /// `org.freedesktop.PolicyKit1.Authority.CheckAuthorization`.
    pub fn check_authorization_blocking(
        &self,
        action_id: &str,
        allow_user_interaction: bool,
    ) -> Result<PolkitAuthResult> {
        if action_id.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "Polkit action_id cannot be empty".into(),
            ));
        }
        let conn = self.0.async_connection(DbusBus::System)?;
        let action = action_id.to_owned();
        let pid = std::process::id();
        let uid = rustix::process::getuid().as_raw();
        let start_time = read_process_start_time(pid).unwrap_or(0);
        let flags: u32 = u32::from(allow_user_interaction);
        futures_lite::future::block_on(async move {
            let mut subj_details: HashMap<&str, Value<'_>> = HashMap::new();
            subj_details.insert("pid", Value::from(pid));
            subj_details.insert("start-time", Value::from(start_time));
            subj_details.insert("uid", Value::from(uid as i32));
            let subject = ("unix-process", subj_details);
            let details: HashMap<&str, &str> = HashMap::new();
            let reply = conn
                .call_method(
                    Some(POLKIT_DEST),
                    POLKIT_AUTHORITY_PATH,
                    Some(POLKIT_AUTHORITY_IFACE),
                    "CheckAuthorization",
                    &(subject, action.as_str(), details, flags, ""),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("Polkit CheckAuthorization failed: {e}"))
                })?;
            let (is_authorized, is_challenge, res_details): (bool, bool, HashMap<String, String>) =
                reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            Ok(PolkitAuthResult {
                is_authorized,
                is_challenge,
                details: res_details,
            })
        })
    }
}

pub(crate) type PolkitReplySlot = AsyncReplySlot<PolkitAgentReply>;
pub(crate) type PendingPolkitMap = Arc<Mutex<HashMap<String, Arc<PolkitReplySlot>>>>;

fn cancel_all_polkit(pending: &PendingPolkitMap) {
    let slots: Vec<Arc<PolkitReplySlot>> = match pending.lock() {
        Ok(mut map) => map.drain().map(|(_, s)| s).collect(),
        Err(poisoned) => poisoned.into_inner().drain().map(|(_, s)| s).collect(),
    };
    for slot in slots {
        slot.resolve(PolkitAgentReply::Cancel);
    }
}

struct PolkitAgentIface {
    sender: calloop::channel::Sender<PolkitAgentEvent>,
    pending: PendingPolkitMap,
    stop: AsyncStopSignal,
}

type RawIdentity = (String, HashMap<String, OwnedValue>);

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl PolkitAgentIface {
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        details: HashMap<String, String>,
        cookie: String,
        identities: Vec<RawIdentity>,
    ) -> zbus::fdo::Result<()> {
        let parsed_ids = parse_polkit_identities(&identities);
        let slot = PolkitReplySlot::new();
        if let Ok(mut map) = self.pending.lock() {
            map.insert(cookie.clone(), Arc::clone(&slot));
        }
        let _ = self
            .sender
            .send(PolkitAgentEvent::BeginAuthentication(Box::new(
                PolkitAuthRequest {
                    cookie: cookie.clone(),
                    action_id,
                    message,
                    icon_name,
                    details,
                    identities: parsed_ids,
                },
            )));

        let reply = futures_lite::future::or(slot.wait(), async {
            self.stop.wait_stopped().await;
            None
        })
        .await
        .unwrap_or(PolkitAgentReply::Cancel);

        if let Ok(mut map) = self.pending.lock() {
            map.remove(&cookie);
        }

        match reply {
            PolkitAgentReply::Complete => {
                let _ = self.sender.send(PolkitAgentEvent::Completed {
                    cookie,
                    authorized: true,
                });
                Ok(())
            }
            PolkitAgentReply::Authenticate { user, response } => {
                // Run blocking polkit-agent-helper-1 pipe I/O off the zbus dispatch thread so
                // concurrent D-Bus calls (such as CancelAuthentication) are never stalled.
                let done_slot = AsyncReplySlot::<bool>::new();
                let done_tx = Arc::clone(&done_slot);
                let sender_clone = self.sender.clone();
                let cookie_clone = cookie.clone();
                let spawned = std::thread::Builder::new()
                    .name("waywi-polkit-helper".into())
                    .spawn(move || {
                        let ok = run_polkit_helper(&user, &cookie_clone, &response, &sender_clone);
                        done_tx.resolve(ok);
                    })
                    .is_ok();
                let ok = if spawned {
                    futures_lite::future::or(done_slot.wait(), async {
                        self.stop.wait_stopped().await;
                        None
                    })
                    .await
                    .unwrap_or(false)
                } else {
                    false
                };
                let _ = self.sender.send(PolkitAgentEvent::Completed {
                    cookie,
                    authorized: ok,
                });
                if ok {
                    Ok(())
                } else {
                    Err(zbus::fdo::Error::AuthFailed(
                        "Polkit authentication failed".into(),
                    ))
                }
            }
            PolkitAgentReply::Cancel => {
                let _ = self.sender.send(PolkitAgentEvent::Completed {
                    cookie,
                    authorized: false,
                });
                Err(zbus::fdo::Error::Failed(
                    "org.freedesktop.PolicyKit1.Error.Cancelled".into(),
                ))
            }
        }
    }

    async fn cancel_authentication(&self, cookie: String) -> zbus::fdo::Result<()> {
        if let Ok(mut map) = self.pending.lock()
            && let Some(slot) = map.remove(&cookie)
        {
            slot.resolve(PolkitAgentReply::Cancel);
        }
        let _ = self
            .sender
            .send(PolkitAgentEvent::CancelAuthentication { cookie });
        Ok(())
    }
}

pub(crate) struct PolkitAgentWorker {
    pending: PendingPolkitMap,
    handle: DbusWorkerHandle,
}

impl PolkitAgentWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<PolkitAgentEvent>,
        pending: PendingPolkitMap,
    ) -> Self {
        let pending_thread = Arc::clone(&pending);
        let handle = DbusWorkerHandle::spawn("waywi-dbus-polkit", move |stop| {
            futures_lite::future::block_on(async move {
                let iface = PolkitAgentIface {
                    sender: sender.clone(),
                    pending: Arc::clone(&pending_thread),
                    stop: stop.clone(),
                };
                let conn_res = match zbus::connection::Builder::system() {
                    Ok(b) => match b.serve_at(POLKIT_AGENT_PATH, iface) {
                        Ok(b2) => b2.build().await,
                        Err(e) => Err(e),
                    },
                    Err(e) => Err(e),
                };
                let conn = match conn_res {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = sender.send(PolkitAgentEvent::Error(format!(
                            "failed to initialize Polkit agent server: {e}"
                        )));
                        return;
                    }
                };

                let session_id = resolve_current_session_id(&conn).await;
                let mut subj_details: HashMap<&str, Value<'_>> = HashMap::new();
                let subject = if let Some(ref sid) = session_id {
                    subj_details.insert("session-id", Value::from(sid.as_str()));
                    ("unix-session", subj_details)
                } else {
                    let pid = std::process::id();
                    subj_details.insert("pid", Value::from(pid));
                    subj_details.insert(
                        "start-time",
                        Value::from(read_process_start_time(pid).unwrap_or(0)),
                    );
                    ("unix-process", subj_details)
                };
                let locale = std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".to_owned());

                if let Err(e) = conn
                    .call_method(
                        Some(POLKIT_DEST),
                        POLKIT_AUTHORITY_PATH,
                        Some(POLKIT_AUTHORITY_IFACE),
                        "RegisterAuthenticationAgent",
                        &(&subject, locale.as_str(), POLKIT_AGENT_PATH),
                    )
                    .await
                {
                    let _ = sender.send(PolkitAgentEvent::Error(format!(
                        "Polkit RegisterAuthenticationAgent failed: {e}"
                    )));
                    return;
                }

                let _ = sender.send(PolkitAgentEvent::Ready);

                // Intercept incoming CancelAuthentication calls directly on the raw stream so
                // an in-flight BeginAuthentication `.await` wakes immediately even while zbus
                // serializes ObjectServer method dispatch.
                let mut raw_stream = zbus::MessageStream::from(&conn);
                use futures_lite::StreamExt;
                while !stop.is_stopped() {
                    let next =
                        futures_lite::future::or(async { Some(raw_stream.next().await) }, async {
                            stop.wait_stopped().await;
                            None
                        })
                        .await;
                    let Some(Some(Ok(msg))) = next else {
                        break;
                    };
                    let header = msg.header();
                    if header.message_type() == zbus::message::Type::MethodCall
                        && header.path().map(|p| p.as_str()) == Some(POLKIT_AGENT_PATH)
                        && header.interface().map(|i| i.as_str())
                            == Some("org.freedesktop.PolicyKit1.AuthenticationAgent")
                        && header.member().map(|m| m.as_str()) == Some("CancelAuthentication")
                        && let Ok(cookie) = msg.body().deserialize::<String>()
                        && let Ok(mut map) = pending_thread.lock()
                        && let Some(slot) = map.remove(&cookie)
                    {
                        slot.resolve(PolkitAgentReply::Cancel);
                    }
                }
                cancel_all_polkit(&pending_thread);

                let _ = conn
                    .call_method(
                        Some(POLKIT_DEST),
                        POLKIT_AUTHORITY_PATH,
                        Some(POLKIT_AUTHORITY_IFACE),
                        "UnregisterAuthenticationAgent",
                        &(&subject, POLKIT_AGENT_PATH),
                    )
                    .await;
            });
        });
        Self { pending, handle }
    }
}

impl Drop for PolkitAgentWorker {
    fn drop(&mut self) {
        self.handle.trigger_stop();
        cancel_all_polkit(&self.pending);
    }
}

async fn resolve_current_session_id(conn: &zbus::Connection) -> Option<String> {
    if let Ok(sid) = std::env::var("XDG_SESSION_ID")
        && !sid.trim().is_empty()
    {
        return Some(sid);
    }
    let reply = conn
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "GetSessionByPID",
            &(std::process::id(),),
        )
        .await
        .ok()?;
    let path: zbus::zvariant::OwnedObjectPath = reply.body().deserialize().ok()?;
    let id_reply = conn
        .call_method(
            Some("org.freedesktop.login1"),
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.login1.Session", "Id"),
        )
        .await
        .ok()?;
    let val: OwnedValue = id_reply.body().deserialize().ok()?;
    match super::zvariant_helpers::unwrap_value(&val) {
        Value::Str(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    }
}

fn read_process_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    // Field 22 (1-based) is starttime; after `)` field 3 is index 0 -> index 19.
    after_comm.split_whitespace().nth(19)?.parse::<u64>().ok()
}

pub(crate) fn parse_polkit_identities(raw: &[RawIdentity]) -> Vec<PolkitIdentity> {
    raw.iter()
        .map(|(kind, details)| {
            let uid = super::zvariant_helpers::dict_u32(details, "uid");
            let user_name = uid.and_then(lookup_username_by_uid);
            PolkitIdentity {
                kind: kind.clone(),
                uid,
                user_name,
            }
        })
        .collect()
}

fn lookup_username_by_uid(uid: u32) -> Option<String> {
    let content = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in content.lines() {
        let mut parts = line.split(':');
        let name = parts.next()?;
        let _pw = parts.next()?;
        let line_uid = parts.next()?.parse::<u32>().ok()?;
        if line_uid == uid {
            return Some(name.to_owned());
        }
    }
    None
}

fn find_polkit_helper() -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        "/usr/lib/polkit-1/polkit-agent-helper-1",
        "/usr/libexec/polkit-agent-helper-1",
        "/usr/lib/x86_64-linux-gnu/polkit-1/polkit-agent-helper-1",
        "/usr/lib/aarch64-linux-gnu/polkit-1/polkit-agent-helper-1",
    ];
    CANDIDATES.iter().copied().find(|p| Path::new(p).exists())
}

fn run_polkit_helper(
    user: &str,
    cookie: &str,
    response: &str,
    sender: &calloop::channel::Sender<PolkitAgentEvent>,
) -> bool {
    let Some(helper) = find_polkit_helper() else {
        let _ = sender.send(PolkitAgentEvent::HelperError {
            cookie: cookie.to_owned(),
            message: "polkit-agent-helper-1 binary not found".into(),
        });
        return false;
    };
    let Ok(mut child) = Command::new(helper)
        .arg(user)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return false;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return false;
    };

    if writeln!(stdin, "{cookie}").is_err() || stdin.flush().is_err() {
        let _ = child.kill();
        return false;
    }

    let mut sent_response = false;
    let mut authorized = false;
    let reader = BufReader::new(stdout);
    for line in reader.lines().map_while(std::result::Result::ok) {
        let trimmed = line.trim();
        if let Some(prompt) = trimmed.strip_prefix("PAM_PROMPT_ECHO_OFF ") {
            let _ = sender.send(PolkitAgentEvent::HelperPrompt {
                cookie: cookie.to_owned(),
                prompt: prompt.to_owned(),
                echo: false,
            });
            if !sent_response {
                let _ = writeln!(stdin, "{response}");
                let _ = stdin.flush();
                sent_response = true;
            }
        } else if let Some(prompt) = trimmed.strip_prefix("PAM_PROMPT_ECHO_ON ") {
            let _ = sender.send(PolkitAgentEvent::HelperPrompt {
                cookie: cookie.to_owned(),
                prompt: prompt.to_owned(),
                echo: true,
            });
            if !sent_response {
                let _ = writeln!(stdin, "{response}");
                let _ = stdin.flush();
                sent_response = true;
            }
        } else if let Some(msg) = trimmed.strip_prefix("PAM_TEXT_INFO ") {
            let _ = sender.send(PolkitAgentEvent::HelperInfo {
                cookie: cookie.to_owned(),
                message: msg.to_owned(),
            });
        } else if let Some(msg) = trimmed.strip_prefix("PAM_ERROR_MSG ") {
            let _ = sender.send(PolkitAgentEvent::HelperError {
                cookie: cookie.to_owned(),
                message: msg.to_owned(),
            });
        } else if trimmed == "SUCCESS" {
            authorized = true;
            break;
        } else if trimmed == "FAILURE" {
            authorized = false;
            break;
        }
    }
    drop(stdin);
    let _ = child.wait();
    authorized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_polkit_unix_user_identities() {
        let mut details: HashMap<String, OwnedValue> = HashMap::new();
        details.insert("uid".into(), OwnedValue::from(0u32));
        let raw = vec![("unix-user".to_owned(), details)];
        let parsed = parse_polkit_identities(&raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].kind, "unix-user");
        assert_eq!(parsed[0].uid, Some(0));
        assert_eq!(parsed[0].user_name.as_deref(), Some("root"));
    }
}
