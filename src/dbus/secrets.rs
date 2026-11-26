//! Freedesktop Secret Service keyring integration (`org.freedesktop.secrets`).
//!
//! Uses standard `plain` session negotiation over the local Unix session bus to search, read,
//! store, and delete secrets in GNOME Keyring, KWallet, or KeePassXC without external crypto
//! crate dependencies.

use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::error::{Result, WaywiError};

use super::client::{DbusActionLane, DbusClient, LaneActionSpec, validate_object_path};
use super::types::DbusBus;
use super::zvariant_helpers::{dict_bool, dict_str, dict_u64, get_all};

const SECRETS_DEST: &str = "org.freedesktop.secrets";
const SECRETS_SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SECRETS_DEFAULT_COLLECTION: &str = "/org/freedesktop/secrets/aliases/default";
const SECRETS_SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const SECRETS_COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const SECRETS_ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const SECRETS_SESSION_IFACE: &str = "org.freedesktop.Secret.Session";

/// Metadata for a keyring item (`org.freedesktop.Secret.Item`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SecretItemInfo {
    pub object_path: String,
    pub label: String,
    pub attributes: HashMap<String, String>,
    pub locked: bool,
    pub created: u64,
    pub modified: u64,
}

/// Zero-cost domain handle for `org.freedesktop.secrets`.
#[derive(Clone, Copy)]
pub struct SecretsClient<'a>(pub(super) &'a DbusClient);

impl SecretsClient<'_> {
    /// Lists collection object paths exposed by `org.freedesktop.Secret.Service.Collections`.
    pub fn list_collections_blocking(&self) -> Result<Vec<String>> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        futures_lite::future::block_on(async move {
            let props = get_all(
                &conn,
                SECRETS_DEST,
                SECRETS_SERVICE_PATH,
                SECRETS_SERVICE_IFACE,
            )
            .await?;
            Ok(super::zvariant_helpers::dict_str_vec(&props, "Collections"))
        })
    }

    /// Searches unlocked and locked items matching `attributes` via `Service.SearchItems`.
    pub fn search_items_blocking(
        &self,
        attributes: &HashMap<String, String>,
    ) -> Result<Vec<SecretItemInfo>> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let attrs = attributes.clone();
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(SECRETS_DEST),
                    SECRETS_SERVICE_PATH,
                    Some(SECRETS_SERVICE_IFACE),
                    "SearchItems",
                    &(attrs,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Secrets.SearchItems failed: {e}")))?;
            let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;

            let mut out = Vec::new();
            for path in unlocked.into_iter().chain(locked).take(128) {
                if let Ok(props) =
                    get_all(&conn, SECRETS_DEST, path.as_str(), SECRETS_ITEM_IFACE).await
                {
                    let mut item_attrs = HashMap::new();
                    if let Some(Value::Dict(d)) = props
                        .get("Attributes")
                        .map(|v| super::zvariant_helpers::unwrap_value(v))
                    {
                        for (k, v) in d.iter() {
                            if let (Value::Str(ks), Value::Str(vs)) = (
                                super::zvariant_helpers::unwrap_value(k),
                                super::zvariant_helpers::unwrap_value(v),
                            ) {
                                item_attrs.insert(ks.to_string(), vs.to_string());
                            }
                        }
                    }
                    out.push(SecretItemInfo {
                        object_path: path.to_string(),
                        label: dict_str(&props, "Label").unwrap_or_default(),
                        attributes: item_attrs,
                        locked: dict_bool(&props, "Locked").unwrap_or(false),
                        created: dict_u64(&props, "Created").unwrap_or(0),
                        modified: dict_u64(&props, "Modified").unwrap_or(0),
                    });
                }
            }
            Ok(out)
        })
    }

    /// Reads the raw secret bytes of an unlocked keyring item (`Item.GetSecret`).
    pub fn get_secret_blocking(&self, item_path: &str) -> Result<Vec<u8>> {
        let path = validate_object_path(item_path, "Secret item path")?;
        let conn = self.0.async_connection(DbusBus::Session)?;
        futures_lite::future::block_on(async move {
            let session = open_plain_session(&conn).await?;
            let res = async {
                let reply = conn
                    .call_method(
                        Some(SECRETS_DEST),
                        path.as_str(),
                        Some(SECRETS_ITEM_IFACE),
                        "GetSecret",
                        &(&session,),
                    )
                    .await
                    .map_err(|e| {
                        WaywiError::DbusError(format!("Secrets.Item.GetSecret failed: {e}"))
                    })?;
                let (_sess, _params, value, _content_type): (
                    OwnedObjectPath,
                    Vec<u8>,
                    Vec<u8>,
                    String,
                ) = reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                Ok(value)
            }
            .await;
            close_session(&conn, &session).await;
            res
        })
    }

    /// Stores or replaces a secret in the default keyring collection
    /// (`/org/freedesktop/secrets/aliases/default`) and returns the created item object path.
    pub fn store_secret_blocking(
        &self,
        label: &str,
        attributes: &HashMap<String, String>,
        secret: &[u8],
        content_type: &str,
        replace: bool,
    ) -> Result<String> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let label = label.to_owned();
        let attrs = attributes.clone();
        let secret = secret.to_vec();
        let ctype = if content_type.is_empty() {
            "text/plain; charset=utf8".to_owned()
        } else {
            content_type.to_owned()
        };
        futures_lite::future::block_on(async move {
            let session = open_plain_session(&conn).await?;
            let res = async {
                let mut props: HashMap<&str, Value<'_>> = HashMap::new();
                props.insert(
                    "org.freedesktop.Secret.Item.Label",
                    Value::from(label.as_str()),
                );
                props.insert("org.freedesktop.Secret.Item.Attributes", Value::from(attrs));
                let secret_tuple = (&session, Vec::<u8>::new(), secret, ctype.as_str());
                let reply = conn
                    .call_method(
                        Some(SECRETS_DEST),
                        SECRETS_DEFAULT_COLLECTION,
                        Some(SECRETS_COLLECTION_IFACE),
                        "CreateItem",
                        &(props, secret_tuple, replace),
                    )
                    .await
                    .map_err(|e| {
                        WaywiError::DbusError(format!("Secrets.CreateItem failed: {e}"))
                    })?;
                let (item_path, _prompt): (OwnedObjectPath, OwnedObjectPath) = reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?;
                Ok(item_path.to_string())
            }
            .await;
            close_session(&conn, &session).await;
            res
        })
    }

    /// Deletes a keyring item asynchronously (`Item.Delete`).
    pub fn delete_item(&self, item_path: &str) -> Result<()> {
        let path = validate_object_path(item_path, "Secret item path")?;
        let target = path.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "Secret.Item.Delete",
                target,
            ),
            move |conn| async move {
                conn.call_method(
                    Some(SECRETS_DEST),
                    path.as_str(),
                    Some(SECRETS_ITEM_IFACE),
                    "Delete",
                    &(),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Secret.Item.Delete failed: {e}")))?;
                Ok(())
            },
        )
    }
}

async fn open_plain_session(conn: &zbus::Connection) -> Result<OwnedObjectPath> {
    let reply = conn
        .call_method(
            Some(SECRETS_DEST),
            SECRETS_SERVICE_PATH,
            Some(SECRETS_SERVICE_IFACE),
            "OpenSession",
            &("plain", Value::from("")),
        )
        .await
        .map_err(|e| WaywiError::DbusError(format!("Secrets.OpenSession failed: {e}")))?;
    let (_output, session_path): (OwnedValue, OwnedObjectPath) = reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
    Ok(session_path)
}

async fn close_session(conn: &zbus::Connection, session: &OwnedObjectPath) {
    let _ = conn
        .call_method(
            Some(SECRETS_DEST),
            session.as_str(),
            Some(SECRETS_SESSION_IFACE),
            "Close",
            &(),
        )
        .await;
}
