//! D-Bus ScreenSaver (`org.freedesktop.ScreenSaver`) and GeoClue2 location-in-use
//! (`org.freedesktop.GeoClue2.Manager`) monitoring.

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{dict_bool, get_all};

const SCREENSAVER_DEST: &str = "org.freedesktop.ScreenSaver";
const SCREENSAVER_PATH: &str = "/org/freedesktop/ScreenSaver";
const SCREENSAVER_IFACE: &str = "org.freedesktop.ScreenSaver";

const GEOCLUE_DEST: &str = "org.freedesktop.GeoClue2";
const GEOCLUE_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const GEOCLUE_IFACE: &str = "org.freedesktop.GeoClue2.Manager";

/// Current `org.freedesktop.ScreenSaver` activation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScreenSaverSnapshot {
    pub active: bool,
}

/// Current `org.freedesktop.GeoClue2.Manager.InUse` location access flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GeoClueSnapshot {
    pub in_use: bool,
}

impl Dbus {
    /// Subscribes to `org.freedesktop.ScreenSaver` activation changes on the session bus.
    pub fn screensaver<M: 'static>(
        map: impl Fn(ScreenSaverSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusScreenSaver {
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Subscribes to `org.freedesktop.GeoClue2.Manager.InUse` location indicator changes.
    pub fn geoclue<M: 'static>(map: impl Fn(GeoClueSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusGeoClue {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for `org.freedesktop.ScreenSaver` state and idle inhibition.
#[derive(Clone, Copy)]
pub struct ScreenSaverClient<'a>(pub(super) &'a DbusClient);

impl ScreenSaverClient<'_> {
    /// Queries synchronously whether `org.freedesktop.ScreenSaver` reports active lock/blanking.
    pub fn active_blocking(&self) -> Result<bool> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        futures_lite::future::block_on(query_screensaver_active(&conn))
    }

    /// Locks the desktop screensaver via `org.freedesktop.ScreenSaver.Lock`.
    pub fn lock(&self) -> Result<()> {
        self.0.call_lane_method_unit(
            DbusActionLane::Desktop,
            DbusBus::Session,
            SCREENSAVER_DEST,
            SCREENSAVER_PATH,
            SCREENSAVER_IFACE,
            "Lock",
        )
    }

    /// Inhibits idle screensaver activation synchronously and returns the cookie for
    /// [`Self::uninhibit`].
    pub fn inhibit_blocking(&self, app: &str, reason: &str) -> Result<u32> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let app = app.to_owned();
        let reason = reason.to_owned();
        futures_lite::future::block_on(async move {
            let reply = conn
                .call_method(
                    Some(SCREENSAVER_DEST),
                    SCREENSAVER_PATH,
                    Some(SCREENSAVER_IFACE),
                    "Inhibit",
                    &(app.as_str(), reason.as_str()),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("ScreenSaver.Inhibit failed: {e}")))?;
            reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))
        })
    }

    /// Releases a previously acquired `org.freedesktop.ScreenSaver` inhibit cookie.
    pub fn uninhibit(&self, cookie: u32) -> Result<()> {
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "ScreenSaver.UnInhibit",
                SCREENSAVER_PATH,
            ),
            move |conn| async move {
                conn.call_method(
                    Some(SCREENSAVER_DEST),
                    SCREENSAVER_PATH,
                    Some(SCREENSAVER_IFACE),
                    "UnInhibit",
                    &(cookie,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("ScreenSaver.UnInhibit failed: {e}")))?;
                Ok(())
            },
        )
    }
}

/// Zero-cost domain handle for `org.freedesktop.GeoClue2` location access queries.
#[derive(Clone, Copy)]
pub struct GeoClueClient<'a>(pub(super) &'a DbusClient);

impl GeoClueClient<'_> {
    /// Reads `org.freedesktop.GeoClue2.Manager.InUse` synchronously from the system bus.
    pub fn in_use_blocking(&self) -> Result<bool> {
        let val = self.0.get_property(
            DbusBus::System,
            GEOCLUE_DEST,
            GEOCLUE_PATH,
            GEOCLUE_IFACE,
            "InUse",
        )?;
        Ok(val.as_bool().unwrap_or(false))
    }
}

async fn query_screensaver_active(conn: &zbus::Connection) -> Result<bool> {
    let reply = conn
        .call_method(
            Some(SCREENSAVER_DEST),
            SCREENSAVER_PATH,
            Some(SCREENSAVER_IFACE),
            "GetActive",
            &(),
        )
        .await
        .map_err(|e| WaywiError::DbusError(format!("ScreenSaver.GetActive failed: {e}")))?;
    reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))
}

pub(crate) fn spawn_screensaver_worker(
    sender: calloop::channel::Sender<ScreenSaverSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::Session,
        "waywi-dbus-screensaver",
        None,
        Some(SCREENSAVER_IFACE),
        sender,
        shared,
        |conn| async move {
            let active = query_screensaver_active(&conn).await.ok()?;
            Some(ScreenSaverSnapshot { active })
        },
    )
}

pub(crate) fn spawn_geoclue_worker(
    sender: calloop::channel::Sender<GeoClueSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-geoclue",
        Some(GEOCLUE_DEST),
        None,
        sender,
        shared,
        |conn| async move {
            let props = get_all(&conn, GEOCLUE_DEST, GEOCLUE_PATH, GEOCLUE_IFACE)
                .await
                .ok()?;
            let in_use = dict_bool(&props, "InUse").unwrap_or(false);
            Some(GeoClueSnapshot { in_use })
        },
    )
}
