//! Typed MPRIS media player helpers (`org.mpris.MediaPlayer2`).

use crate::error::{Result, WaywiError};

use super::client::DbusClient;
use super::types::{DbusBus, DbusValue};

/// Typed MPRIS playback status (`org.mpris.MediaPlayer2.Player`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MprisPlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

/// Typed snapshot of an MPRIS media player.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MprisSnapshot {
    pub service_name: String,
    pub status: MprisPlaybackStatus,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_OBJECT: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

impl DbusClient {
    /// Lists active MPRIS media player bus names (`org.mpris.MediaPlayer2.*`).
    pub fn list_mpris_players(&self) -> Result<Vec<String>> {
        let conn = self.connection(DbusBus::Session)?;
        let dbus = zbus::blocking::fdo::DBusProxy::new(&conn)
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        let names = dbus
            .list_names()
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        Ok(names
            .into_iter()
            .map(|n| n.to_string())
            .filter(|n| n.starts_with(MPRIS_PREFIX))
            .collect())
    }

    /// Reads a typed [`MprisSnapshot`] from `service_name` (e.g. `"org.mpris.MediaPlayer2.spotify"`).
    pub fn mpris_snapshot(&self, service_name: &str) -> Result<MprisSnapshot> {
        let conn = self.connection(DbusBus::Session)?;
        let props = super::zvariant_helpers::get_all_blocking(
            &conn,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
        )?;
        let status = match super::zvariant_helpers::dict_str(&props, "PlaybackStatus")
            .as_deref()
            .unwrap_or("Stopped")
        {
            "Playing" => MprisPlaybackStatus::Playing,
            "Paused" => MprisPlaybackStatus::Paused,
            _ => MprisPlaybackStatus::Stopped,
        };
        let meta = props
            .get("Metadata")
            .map(super::client::owned_value_to_dbus);

        let (title, artist, album) = if let Some(DbusValue::Dict(map)) = meta {
            let title = map
                .get("xesam:title")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let album = map
                .get("xesam:album")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let artist = map.get("xesam:artist").and_then(|v| match v {
                DbusValue::Array(arr) => arr.first().and_then(|x| x.as_str()).map(str::to_owned),
                DbusValue::String(s) => Some(s.clone()),
                _ => None,
            });
            (title, artist, album)
        } else {
            (None, None, None)
        };

        Ok(MprisSnapshot {
            service_name: service_name.to_owned(),
            status,
            title,
            artist,
            album,
        })
    }

    /// Invokes `org.mpris.MediaPlayer2.Player.PlayPause` on `service_name`.
    pub fn mpris_play_pause(&self, service_name: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
            "PlayPause",
        )
    }

    /// Invokes `org.mpris.MediaPlayer2.Player.Next` on `service_name`.
    pub fn mpris_next(&self, service_name: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
            "Next",
        )
    }

    /// Invokes `org.mpris.MediaPlayer2.Player.Previous` on `service_name`.
    pub fn mpris_previous(&self, service_name: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
            "Previous",
        )
    }
}
