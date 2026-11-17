//! D-Bus ScreenSaver (`org.freedesktop.ScreenSaver`) and GeoClue2 location-in-use
//! (`org.freedesktop.GeoClue2.Manager`) monitoring.

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;

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
                key: "dbus:screensaver".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Subscribes to `org.freedesktop.GeoClue2.Manager.InUse` location indicator changes.
    pub fn geoclue<M: 'static>(map: impl Fn(GeoClueSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusGeoClue {
                key: "dbus:geoclue2".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Queries whether `org.freedesktop.ScreenSaver` reports the screensaver/lock as active.
    pub fn screensaver_active(&self) -> Result<bool> {
        let conn = self.connection(DbusBus::Session)?;
        let reply = conn
            .call_method(
                Some(SCREENSAVER_DEST),
                SCREENSAVER_PATH,
                Some(SCREENSAVER_IFACE),
                "GetActive",
                &(),
            )
            .map_err(|e| WaywiError::DbusError(format!("ScreenSaver.GetActive failed: {e}")))?;
        reply
            .body()
            .deserialize()
            .map_err(|e| WaywiError::DbusError(e.to_string()))
    }

    /// Locks the desktop screensaver via `org.freedesktop.ScreenSaver.Lock`.
    pub fn lock_screensaver(&self) -> Result<()> {
        self.call_method_unit(
            DbusBus::Session,
            SCREENSAVER_DEST,
            SCREENSAVER_PATH,
            SCREENSAVER_IFACE,
            "Lock",
        )
    }

    /// Inhibits idle screensaver activation and returns the cookie for [`Self::uninhibit_screensaver`].
    pub fn inhibit_screensaver(&self, app_name: &str, reason: &str) -> Result<u32> {
        let conn = self.connection(DbusBus::Session)?;
        let reply = conn
            .call_method(
                Some(SCREENSAVER_DEST),
                SCREENSAVER_PATH,
                Some(SCREENSAVER_IFACE),
                "Inhibit",
                &(app_name, reason),
            )
            .map_err(|e| WaywiError::DbusError(format!("ScreenSaver.Inhibit failed: {e}")))?;
        reply
            .body()
            .deserialize()
            .map_err(|e| WaywiError::DbusError(e.to_string()))
    }

    /// Releases a previously acquired `org.freedesktop.ScreenSaver` inhibit cookie.
    pub fn uninhibit_screensaver(&self, cookie: u32) -> Result<()> {
        self.submit_lane_action(
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

    /// Reads `org.freedesktop.GeoClue2.Manager.InUse` from the system bus.
    pub fn geoclue_in_use(&self) -> Result<bool> {
        let val = self.get_property(
            DbusBus::System,
            GEOCLUE_DEST,
            GEOCLUE_PATH,
            GEOCLUE_IFACE,
            "InUse",
        )?;
        Ok(val.as_bool().unwrap_or(false))
    }
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
            let reply = conn
                .call_method(
                    Some(SCREENSAVER_DEST),
                    SCREENSAVER_PATH,
                    Some(SCREENSAVER_IFACE),
                    "GetActive",
                    &(),
                )
                .await
                .ok()?;
            let active: bool = reply.body().deserialize().ok()?;
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
            let props = super::zvariant_helpers::get_all_async(
                &conn,
                GEOCLUE_DEST,
                GEOCLUE_PATH,
                GEOCLUE_IFACE,
            )
            .await?;
            let in_use = super::zvariant_helpers::dict_bool(&props, "InUse").unwrap_or(false);
            Some(GeoClueSnapshot { in_use })
        },
    )
}
