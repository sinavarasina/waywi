//! Feral GameMode status and game registration integration (`com.feralinteractive.GameMode`).

use zbus::zvariant::OwnedObjectPath;

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{dict_i32, get_all};

const GAMEMODE_DEST: &str = "com.feralinteractive.GameMode";
const GAMEMODE_PATH: &str = "/com/feralinteractive/GameMode";
const GAMEMODE_IFACE: &str = "com.feralinteractive.GameMode";

/// Snapshot of active Feral GameMode state and registered game PIDs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GameModeSnapshot {
    pub client_count: i32,
    pub active: bool,
    pub games: Vec<(i32, String)>,
}

impl Dbus {
    /// Subscribes to `com.feralinteractive.GameMode` active client changes on the session bus.
    pub fn gamemode<M: 'static>(map: impl Fn(GameModeSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusGameMode {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for `com.feralinteractive.GameMode`.
#[derive(Clone, Copy)]
pub struct GameModeClient<'a>(pub(super) &'a DbusClient);

impl GameModeClient<'_> {
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<GameModeSnapshot> {
        self.0.cache.gamemode.read().ok().and_then(|g| g.clone())
    }

    pub fn snapshot_blocking(&self) -> Result<GameModeSnapshot> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        futures_lite::future::block_on(read_gamemode_snapshot(&conn))
    }

    /// Queries GameMode status for `pid` (`0` = inactive, `1` = active, `2` = active & registered).
    pub fn query_status_blocking(&self, pid: i32) -> Result<i32> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(GAMEMODE_DEST),
                    GAMEMODE_PATH,
                    Some(GAMEMODE_IFACE),
                    "QueryStatus",
                    &(pid,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("GameMode.QueryStatus failed: {e}")))?;
            reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))
        })
    }

    /// Registers `pid` with GameMode (`RegisterGame`).
    pub fn register_game(&self, pid: i32) -> Result<()> {
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "GameMode.RegisterGame",
                pid.to_string(),
            ),
            move |conn| async move {
                conn.call_method(
                    Some(GAMEMODE_DEST),
                    GAMEMODE_PATH,
                    Some(GAMEMODE_IFACE),
                    "RegisterGame",
                    &(pid,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("GameMode.RegisterGame failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Unregisters `pid` from GameMode (`UnregisterGame`).
    pub fn unregister_game(&self, pid: i32) -> Result<()> {
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "GameMode.UnregisterGame",
                pid.to_string(),
            ),
            move |conn| async move {
                conn.call_method(
                    Some(GAMEMODE_DEST),
                    GAMEMODE_PATH,
                    Some(GAMEMODE_IFACE),
                    "UnregisterGame",
                    &(pid,),
                )
                .await
                .map_err(|e| {
                    WaywiError::DbusError(format!("GameMode.UnregisterGame failed: {e}"))
                })?;
                Ok(())
            },
        )
    }
}

async fn read_gamemode_snapshot(conn: &zbus::Connection) -> Result<GameModeSnapshot> {
    let props = get_all(conn, GAMEMODE_DEST, GAMEMODE_PATH, GAMEMODE_IFACE).await?;
    let client_count = dict_i32(&props, "ClientCount").unwrap_or(0);
    let games = match conn
        .call_method(
            Some(GAMEMODE_DEST),
            GAMEMODE_PATH,
            Some(GAMEMODE_IFACE),
            "ListGames",
            &(),
        )
        .await
    {
        Ok(reply) => reply
            .body()
            .deserialize::<Vec<(i32, OwnedObjectPath)>>()
            .map(|v| v.into_iter().map(|(pid, p)| (pid, p.to_string())).collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    Ok(GameModeSnapshot {
        active: client_count > 0 || !games.is_empty(),
        client_count,
        games,
    })
}

pub(crate) fn spawn_gamemode_worker(
    sender: calloop::channel::Sender<GameModeSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::Session,
        "waywi-dbus-gamemode",
        Some(GAMEMODE_DEST),
        None,
        sender,
        shared,
        |conn| async move { read_gamemode_snapshot(&conn).await.ok() },
    )
}
