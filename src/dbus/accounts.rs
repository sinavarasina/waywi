//! AccountsService user profile integration (`org.freedesktop.Accounts`).

use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{
    DBUS_SLOW_TIMEOUT, DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec,
    validate_object_path,
};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{dict_bool, dict_i32, dict_str, dict_u64, get_all};

const ACCOUNTS_DEST: &str = "org.freedesktop.Accounts";
const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
const ACCOUNTS_IFACE: &str = "org.freedesktop.Accounts";
const ACCOUNTS_USER_IFACE: &str = "org.freedesktop.Accounts.User";

/// User privilege level reported by `org.freedesktop.Accounts.User.AccountType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AccountType {
    #[default]
    Standard = 0,
    Administrator = 1,
}

impl AccountType {
    #[must_use]
    pub const fn from_i32(raw: i32) -> Self {
        match raw {
            1 => Self::Administrator,
            _ => Self::Standard,
        }
    }
}

/// User profile descriptor from `org.freedesktop.Accounts.User`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserAccount {
    pub object_path: String,
    pub uid: u64,
    pub user_name: String,
    pub real_name: String,
    pub icon_file: String,
    pub home_directory: String,
    pub shell: String,
    pub account_type: AccountType,
    pub locked: bool,
    pub automatic_login: bool,
    pub system_account: bool,
}

impl UserAccount {
    /// Returns `real_name` when non-empty, falling back to `user_name`.
    #[must_use]
    pub fn display_name(&self) -> &str {
        if !self.real_name.trim().is_empty() {
            &self.real_name
        } else {
            &self.user_name
        }
    }
}

/// Snapshot of user accounts reported by `org.freedesktop.Accounts`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountsSnapshot {
    pub users: Vec<UserAccount>,
}

impl AccountsSnapshot {
    #[must_use]
    pub fn by_uid(&self, uid: u64) -> Option<&UserAccount> {
        self.users.iter().find(|u| u.uid == uid)
    }

    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&UserAccount> {
        self.users.iter().find(|u| u.user_name == name)
    }

    /// Returns the account matching the current process's real UID, if listed.
    #[must_use]
    pub fn current_user(&self) -> Option<&UserAccount> {
        let uid = u64::from(rustix::process::getuid().as_raw());
        self.by_uid(uid)
    }
}

impl Dbus {
    /// Subscribes to `org.freedesktop.Accounts` user profile changes on the system bus.
    pub fn accounts<M: 'static>(map: impl Fn(AccountsSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusAccounts {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for `org.freedesktop.Accounts`.
#[derive(Clone, Copy)]
pub struct AccountsClient<'a>(pub(super) &'a DbusClient);

impl AccountsClient<'_> {
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<AccountsSnapshot> {
        self.0.cache.accounts.read().ok().and_then(|g| g.clone())
    }

    pub fn snapshot_blocking(&self) -> Result<AccountsSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(read_accounts_snapshot(&conn))
    }

    /// Queries the current process user (`FindUserById(getuid())`) synchronously.
    pub fn current_user_blocking(&self) -> Result<UserAccount> {
        let conn = self.0.async_connection(DbusBus::System)?;
        let uid = i64::from(rustix::process::getuid().as_raw());
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(ACCOUNTS_DEST),
                    ACCOUNTS_PATH,
                    Some(ACCOUNTS_IFACE),
                    "FindUserById",
                    &(uid,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Accounts.FindUserById failed: {e}")))?;
            let path: OwnedObjectPath = reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            let props = get_all(&conn, ACCOUNTS_DEST, path.as_str(), ACCOUNTS_USER_IFACE).await?;
            Ok(parse_user_account(path.to_string(), &props))
        })
    }

    /// Updates a user's avatar file path (`org.freedesktop.Accounts.User.SetIconFile`).
    pub fn set_icon_file(&self, user_path: &str, icon_file: &str) -> Result<()> {
        let path = validate_object_path(user_path, "Accounts user path")?;
        let icon = icon_file.to_owned();
        let target = path.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Accounts.User.SetIconFile",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(ACCOUNTS_DEST),
                    path.as_str(),
                    Some(ACCOUNTS_USER_IFACE),
                    "SetIconFile",
                    &(icon.as_str(),),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("User.SetIconFile failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Updates a user's display name (`org.freedesktop.Accounts.User.SetRealName`).
    pub fn set_real_name(&self, user_path: &str, real_name: &str) -> Result<()> {
        let path = validate_object_path(user_path, "Accounts user path")?;
        let name = real_name.to_owned();
        let target = path.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Accounts.User.SetRealName",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(ACCOUNTS_DEST),
                    path.as_str(),
                    Some(ACCOUNTS_USER_IFACE),
                    "SetRealName",
                    &(name.as_str(),),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("User.SetRealName failed: {e}")))?;
                Ok(())
            },
        )
    }
}

async fn read_accounts_snapshot(conn: &zbus::Connection) -> Result<AccountsSnapshot> {
    let reply = conn
        .call_method(
            Some(ACCOUNTS_DEST),
            ACCOUNTS_PATH,
            Some(ACCOUNTS_IFACE),
            "ListCachedUsers",
            &(),
        )
        .await
        .map_err(|e| WaywiError::DbusError(format!("Accounts.ListCachedUsers failed: {e}")))?;
    let paths: Vec<OwnedObjectPath> = reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
    let mut users = Vec::new();
    for path in paths.into_iter().take(64) {
        if let Ok(props) = get_all(conn, ACCOUNTS_DEST, path.as_str(), ACCOUNTS_USER_IFACE).await {
            users.push(parse_user_account(path.to_string(), &props));
        }
    }
    users.sort_by_key(|u| u.uid);
    Ok(AccountsSnapshot { users })
}

pub(crate) fn spawn_accounts_worker(
    sender: calloop::channel::Sender<AccountsSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-accounts",
        Some(ACCOUNTS_DEST),
        None,
        sender,
        shared,
        |conn| async move { read_accounts_snapshot(&conn).await.ok() },
    )
}

pub(crate) fn parse_user_account(
    object_path: String,
    props: &HashMap<String, OwnedValue>,
) -> UserAccount {
    UserAccount {
        object_path,
        uid: dict_u64(props, "Uid").unwrap_or(0),
        user_name: dict_str(props, "UserName").unwrap_or_default(),
        real_name: dict_str(props, "RealName").unwrap_or_default(),
        icon_file: dict_str(props, "IconFile").unwrap_or_default(),
        home_directory: dict_str(props, "HomeDirectory").unwrap_or_default(),
        shell: dict_str(props, "Shell").unwrap_or_default(),
        account_type: AccountType::from_i32(dict_i32(props, "AccountType").unwrap_or(0)),
        locked: dict_bool(props, "Locked").unwrap_or(false),
        automatic_login: dict_bool(props, "AutomaticLogin").unwrap_or(false),
        system_account: dict_bool(props, "SystemAccount").unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn parses_accounts_user_properties() {
        let mut props = HashMap::new();
        props.insert("Uid".into(), OwnedValue::from(1000u64));
        props.insert(
            "UserName".into(),
            OwnedValue::try_from(Value::from("sina")).unwrap(),
        );
        props.insert(
            "RealName".into(),
            OwnedValue::try_from(Value::from("Sina")).unwrap(),
        );
        props.insert("AccountType".into(), OwnedValue::from(1i32));
        props.insert(
            "IconFile".into(),
            OwnedValue::try_from(Value::from("/var/lib/AccountsService/icons/sina")).unwrap(),
        );

        let user = parse_user_account("/org/freedesktop/Accounts/User1000".into(), &props);
        assert_eq!(user.uid, 1000);
        assert_eq!(user.display_name(), "Sina");
        assert_eq!(user.account_type, AccountType::Administrator);
    }
}
