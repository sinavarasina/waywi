//! XDG Desktop Portal Global Shortcuts (`org.freedesktop.portal.GlobalShortcuts`) support.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::DbusClient;
use super::types::{Dbus, DbusBus};
use super::worker::AsyncStopSignal;

const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_GS_IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
const PORTAL_REQ_IFACE: &str = "org.freedesktop.portal.Request";
const PORTAL_SESSION_IFACE: &str = "org.freedesktop.portal.Session";

static TOKEN_SEQ: AtomicU64 = AtomicU64::new(1);

/// Shortcut definition registered with `org.freedesktop.portal.GlobalShortcuts`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GlobalShortcutDescriptor {
    pub id: String,
    pub description: String,
    pub preferred_trigger: Option<String>,
}

impl GlobalShortcutDescriptor {
    /// Creates a shortcut registration descriptor with a stable `id` and user-visible description.
    #[must_use]
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            preferred_trigger: None,
        }
    }

    /// Suggests a default trigger (e.g. `"LOGO+n"`, `"ALT+SPACE"`, or `"CTRL+ALT+v"`).
    #[must_use]
    pub fn preferred_trigger(mut self, trigger: impl Into<String>) -> Self {
        self.preferred_trigger = Some(trigger.into());
        self
    }
}

/// Bound shortcut metadata returned by the desktop portal compositor integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundGlobalShortcut {
    pub id: String,
    pub description: String,
    pub trigger_description: String,
}

/// Event produced by a [`GlobalShortcuts::subscribe`] subscription.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalShortcutEvent {
    Bound(Vec<BoundGlobalShortcut>),
    Activated { id: String, timestamp: u64 },
    Deactivated { id: String, timestamp: u64 },
    ShortcutsChanged(Vec<BoundGlobalShortcut>),
    Unavailable(String),
    Error(String),
}

/// XDG Desktop Portal global shortcut subscription builder.
pub struct GlobalShortcuts;

impl GlobalShortcuts {
    /// Opens an `org.freedesktop.portal.GlobalShortcuts` session, binds `shortcuts`, and streams
    /// compositor shortcut activations cleanly without global X11 grabs.
    pub fn subscribe<M: 'static>(
        app_id: impl Into<String>,
        shortcuts: Vec<GlobalShortcutDescriptor>,
        map: impl Fn(GlobalShortcutEvent) -> M + 'static,
    ) -> Subscription<M> {
        let app_id = app_id.into();
        let mut shortcut_keys = Vec::with_capacity(shortcuts.len() + 1);
        shortcut_keys.push(app_id.clone());
        for s in &shortcuts {
            shortcut_keys.push(format!(
                "{}:{}:{:?}",
                s.id, s.description, s.preferred_trigger
            ));
        }
        let key = shortcut_keys.join("|").into_subscription_key();
        Subscription {
            spec: SubscriptionSpec::GlobalShortcuts {
                key,
                app_id,
                shortcuts,
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl Dbus {
    /// Convenience alias for [`GlobalShortcuts::subscribe`].
    pub fn global_shortcuts<M: 'static>(
        app_id: impl Into<String>,
        shortcuts: Vec<GlobalShortcutDescriptor>,
        map: impl Fn(GlobalShortcutEvent) -> M + 'static,
    ) -> Subscription<M> {
        GlobalShortcuts::subscribe(app_id, shortcuts, map)
    }
}

impl DbusClient {
    /// Probes whether `org.freedesktop.portal.GlobalShortcuts` is exported by the session portal.
    #[must_use]
    pub fn has_global_shortcuts_portal(&self) -> bool {
        self.get_property(
            DbusBus::Session,
            PORTAL_DEST,
            PORTAL_PATH,
            PORTAL_GS_IFACE,
            "version",
        )
        .is_ok()
    }
}

pub(crate) struct GlobalShortcutsWorker {
    stop: AsyncStopSignal,
    join: Option<std::thread::JoinHandle<()>>,
}

impl GlobalShortcutsWorker {
    pub(crate) fn spawn(
        app_id: String,
        shortcuts: Vec<GlobalShortcutDescriptor>,
        sender: calloop::channel::Sender<GlobalShortcutEvent>,
    ) -> Self {
        let stop = AsyncStopSignal::new();
        let stop_sig = stop.clone();
        let join = std::thread::Builder::new()
            .name("waywi-global-shortcuts".to_owned())
            .spawn(move || {
                futures_lite::future::block_on(run_portal_worker(
                    app_id, shortcuts, sender, stop_sig,
                ));
            })
            .ok();
        Self { stop, join }
    }
}

impl Drop for GlobalShortcutsWorker {
    fn drop(&mut self) {
        self.stop.trigger();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

async fn run_portal_worker(
    app_id: String,
    shortcuts: Vec<GlobalShortcutDescriptor>,
    sender: calloop::channel::Sender<GlobalShortcutEvent>,
    stop_sig: AsyncStopSignal,
) {
    let Ok(conn) = zbus::Connection::session().await else {
        let _ = sender.send(GlobalShortcutEvent::Unavailable(
            "session D-Bus connection unavailable".to_owned(),
        ));
        stop_sig.wait_stopped().await;
        return;
    };

    if conn
        .call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(PORTAL_GS_IFACE, "version"),
        )
        .await
        .is_err()
    {
        let _ = sender.send(GlobalShortcutEvent::Unavailable(
            "org.freedesktop.portal.GlobalShortcuts not available in this session".to_owned(),
        ));
        stop_sig.wait_stopped().await;
        return;
    }

    let sender_uniq = conn
        .unique_name()
        .map(|u| u.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_else(|| "waywi".to_owned());

    let create_token = format!("waywi_gs_req_{}", TOKEN_SEQ.fetch_add(1, Ordering::Relaxed));
    let session_token = format!(
        "waywi_gs_sess_{}",
        TOKEN_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let expected_create_req =
        format!("/org/freedesktop/portal/desktop/request/{sender_uniq}/{create_token}");

    let Ok(req_rule) = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(PORTAL_REQ_IFACE)
        .and_then(|b| b.member("Response"))
        .and_then(|b| b.path(expected_create_req.as_str()))
    else {
        return;
    };
    let Ok(mut req_stream) =
        zbus::MessageStream::for_match_rule(req_rule.build(), &conn, Some(8)).await
    else {
        return;
    };

    let mut create_opts: HashMap<&str, Value<'_>> = HashMap::new();
    create_opts.insert("handle_token", Value::from(create_token.as_str()));
    create_opts.insert("session_handle_token", Value::from(session_token.as_str()));
    if !app_id.is_empty() {
        create_opts.insert("app_id", Value::from(app_id.as_str()));
    }

    if let Err(e) = conn
        .call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some(PORTAL_GS_IFACE),
            "CreateSession",
            &(create_opts,),
        )
        .await
    {
        let _ = sender.send(GlobalShortcutEvent::Error(format!(
            "GlobalShortcuts CreateSession failed: {e}"
        )));
        stop_sig.wait_stopped().await;
        return;
    }

    use futures_lite::StreamExt;
    let create_msg = futures_lite::future::or(async { Some(req_stream.next().await) }, async {
        stop_sig.wait_stopped().await;
        None
    })
    .await;
    let Some(Some(Ok(resp))) = create_msg else {
        return;
    };
    let Ok((code, results)) = resp
        .body()
        .deserialize::<(u32, HashMap<String, OwnedValue>)>()
    else {
        let _ = sender.send(GlobalShortcutEvent::Error(
            "invalid CreateSession portal response".to_owned(),
        ));
        return;
    };
    if code != 0 {
        let _ = sender.send(GlobalShortcutEvent::Error(format!(
            "CreateSession rejected with code {code}"
        )));
        return;
    }

    let session_handle = results
        .get("session_handle")
        .and_then(|v| match &**v {
            Value::Str(s) => Some(s.to_string()),
            Value::ObjectPath(p) => Some(p.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| {
            format!("/org/freedesktop/portal/desktop/session/{sender_uniq}/{session_token}")
        });

    let Ok(session_obj_path) = OwnedObjectPath::try_from(session_handle.clone()) else {
        let _ = sender.send(GlobalShortcutEvent::Error(
            "portal returned invalid session handle path".to_owned(),
        ));
        return;
    };

    // Listen to GlobalShortcuts signals before BindShortcuts completes.
    let Ok(gs_rule) = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(PORTAL_GS_IFACE)
        .and_then(|b| b.path(PORTAL_PATH))
    else {
        return;
    };
    let Ok(mut gs_stream) =
        zbus::MessageStream::for_match_rule(gs_rule.build(), &conn, Some(64)).await
    else {
        return;
    };

    if !shortcuts.is_empty() {
        let bind_token = format!(
            "waywi_gs_bind_{}",
            TOKEN_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let expected_bind_req =
            format!("/org/freedesktop/portal/desktop/request/{sender_uniq}/{bind_token}");
        let bind_rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(PORTAL_REQ_IFACE)
            .and_then(|b| b.member("Response"))
            .and_then(|b| b.path(expected_bind_req.as_str()));

        let mut bind_stream_opt = match bind_rule {
            Ok(r) => zbus::MessageStream::for_match_rule(r.build(), &conn, Some(8))
                .await
                .ok(),
            Err(_) => None,
        };

        let wire_shortcuts: Vec<(String, HashMap<String, Value<'_>>)> = shortcuts
            .iter()
            .map(|s| {
                let mut opts = HashMap::new();
                opts.insert(
                    "description".to_owned(),
                    Value::from(s.description.as_str()),
                );
                if let Some(ref trig) = s.preferred_trigger {
                    opts.insert("preferred_trigger".to_owned(), Value::from(trig.as_str()));
                }
                (s.id.clone(), opts)
            })
            .collect();
        let mut bind_opts: HashMap<&str, Value<'_>> = HashMap::new();
        bind_opts.insert("handle_token", Value::from(bind_token.as_str()));

        match conn
            .call_method(
                Some(PORTAL_DEST),
                PORTAL_PATH,
                Some(PORTAL_GS_IFACE),
                "BindShortcuts",
                &(&session_obj_path, wire_shortcuts, "", bind_opts),
            )
            .await
        {
            Ok(_) => {
                if let Some(ref mut bstream) = bind_stream_opt {
                    let bmsg =
                        futures_lite::future::or(async { Some(bstream.next().await) }, async {
                            stop_sig.wait_stopped().await;
                            None
                        })
                        .await;
                    if let Some(Some(Ok(msg))) = bmsg
                        && let Ok((bcode, bres)) = msg
                            .body()
                            .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                    {
                        if bcode == 0 {
                            let bound = bres
                                .get("shortcuts")
                                .map(parse_bound_shortcuts_value)
                                .unwrap_or_default();
                            let _ = sender.send(GlobalShortcutEvent::Bound(bound));
                        } else {
                            let _ = sender.send(GlobalShortcutEvent::Error(format!(
                                "BindShortcuts returned portal code {bcode}"
                            )));
                        }
                    }
                }
            }
            Err(e) => {
                let _ = sender.send(GlobalShortcutEvent::Error(format!(
                    "BindShortcuts failed: {e}"
                )));
            }
        }
    }

    while !stop_sig.is_stopped() {
        let next_msg = futures_lite::future::or(async { Some(gs_stream.next().await) }, async {
            stop_sig.wait_stopped().await;
            None
        })
        .await;
        let Some(Some(Ok(msg))) = next_msg else {
            break;
        };
        let header = msg.header();
        let member = header.member().map(|m| m.as_str()).unwrap_or("");
        match member {
            "Activated" => {
                if let Ok((sess, id, timestamp, _opts)) =
                    msg.body()
                        .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>(
                        )
                    && sess.as_str() == session_handle
                {
                    let _ = sender.send(GlobalShortcutEvent::Activated { id, timestamp });
                }
            }
            "Deactivated" => {
                if let Ok((sess, id, timestamp, _opts)) =
                    msg.body()
                        .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>(
                        )
                    && sess.as_str() == session_handle
                {
                    let _ = sender.send(GlobalShortcutEvent::Deactivated { id, timestamp });
                }
            }
            "ShortcutsChanged" => {
                if let Ok((sess, raw_list)) = msg
                    .body()
                    .deserialize::<(OwnedObjectPath, Vec<(String, HashMap<String, OwnedValue>)>)>()
                    && sess.as_str() == session_handle
                {
                    let _ = sender.send(GlobalShortcutEvent::ShortcutsChanged(
                        parse_bound_shortcut_pairs(raw_list),
                    ));
                }
            }
            _ => {}
        }
    }

    let _ = conn
        .call_method(
            Some(PORTAL_DEST),
            session_handle.as_str(),
            Some(PORTAL_SESSION_IFACE),
            "Close",
            &(),
        )
        .await;
}

pub(crate) fn parse_bound_shortcuts_value(val: &OwnedValue) -> Vec<BoundGlobalShortcut> {
    if let Ok(pairs) = Vec::<(String, HashMap<String, OwnedValue>)>::try_from(val.clone()) {
        parse_bound_shortcut_pairs(pairs)
    } else {
        Vec::new()
    }
}

pub(crate) fn parse_bound_shortcut_pairs(
    pairs: Vec<(String, HashMap<String, OwnedValue>)>,
) -> Vec<BoundGlobalShortcut> {
    pairs
        .into_iter()
        .map(|(id, opts)| {
            let description = opts
                .get("description")
                .and_then(|v| <&str>::try_from(&**v).ok())
                .unwrap_or_default()
                .to_owned();
            let trigger_description = opts
                .get("trigger_description")
                .and_then(|v| <&str>::try_from(&**v).ok())
                .unwrap_or_default()
                .to_owned();
            BoundGlobalShortcut {
                id,
                description,
                trigger_description,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_portal_bound_shortcut_pairs() {
        let mut opts = HashMap::new();
        opts.insert(
            "description".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from("Toggle Overlay")),
        );
        opts.insert(
            "trigger_description".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from("Super+O")),
        );
        let list = parse_bound_shortcut_pairs(vec![("toggle-overlay".to_owned(), opts)]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "toggle-overlay");
        assert_eq!(list[0].trigger_description, "Super+O");
    }
}
