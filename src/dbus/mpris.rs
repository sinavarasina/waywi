//! Typed MPRIS media player inspection, control, and reactive subscriptions (`org.mpris.MediaPlayer2`).

use std::collections::{HashMap, HashSet};

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{DbusClient, DbusSharedBus, connect_shared_bus, owned_value_to_dbus};
use super::types::{Dbus, DbusBus, DbusValue};
use super::worker::AsyncStopSignal;
use super::zvariant_helpers::{
    dict_bool, dict_f64, dict_i64, dict_str, get_all_async, get_all_blocking,
};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_OBJECT: &str = "/org/mpris/MediaPlayer2";
const MPRIS_ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const MPRIS_PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const FDO_PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const FDO_DBUS_IFACE: &str = "org.freedesktop.DBus";

/// Typed MPRIS playback status (`org.mpris.MediaPlayer2.Player.PlaybackStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MprisPlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

impl MprisPlaybackStatus {
    /// Parses an MPRIS `PlaybackStatus` string.
    #[must_use]
    pub fn from_str_status(val: &str) -> Self {
        match val {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// Typed MPRIS loop / repeat mode (`org.mpris.MediaPlayer2.Player.LoopStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MprisLoopStatus {
    #[default]
    None,
    Track,
    Playlist,
}

impl MprisLoopStatus {
    /// Parses an MPRIS `LoopStatus` string (`"None"`, `"Track"`, `"Playlist"`).
    #[must_use]
    pub fn from_str_status(val: &str) -> Self {
        match val {
            "Track" => Self::Track,
            "Playlist" => Self::Playlist,
            _ => Self::None,
        }
    }

    /// Returns the canonical MPRIS `LoopStatus` wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Track => "Track",
            Self::Playlist => "Playlist",
        }
    }
}

/// Typed snapshot of an MPRIS media player (`org.mpris.MediaPlayer2` + `org.mpris.MediaPlayer2.Player`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MprisSnapshot {
    pub service_name: String,
    pub identity: Option<String>,
    pub desktop_entry: Option<String>,
    pub status: MprisPlaybackStatus,
    pub loop_status: MprisLoopStatus,
    pub shuffle: bool,
    pub volume_percent: Option<u8>,
    pub track_id: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub art_url: Option<String>,
    pub length_us: Option<i64>,
    pub position_us: Option<i64>,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
    pub can_control: bool,
    pub can_raise: bool,
    pub can_quit: bool,
}

/// Events emitted by [`Dbus::mpris`] when observing session media players.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MprisEvent {
    /// Initial or updated aggregate list of active MPRIS players sorted by priority (`Playing` first).
    PlayersUpdated(Vec<MprisSnapshot>),
    /// A specific MPRIS player updated its playback status, metadata, position, or capabilities.
    PlayerChanged(Box<MprisSnapshot>),
    /// An MPRIS player bus name appeared on the session bus and exposed its player object.
    PlayerAdded { service_name: String },
    /// An MPRIS player bus name vanished from the session bus.
    PlayerRemoved { service_name: String },
    /// Playback position jumped (`org.mpris.MediaPlayer2.Player.Seeked`).
    Seeked {
        service_name: String,
        position_us: i64,
    },
}

impl Dbus {
    /// Subscribes to session MPRIS media player additions, removals, property updates, and seek events.
    pub fn mpris<M: 'static>(map: impl Fn(MprisEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusMpris {
                key: "dbus:mpris".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Lists active MPRIS media player bus names (`org.mpris.MediaPlayer2.*`).
    pub fn list_mpris_players(&self) -> Result<Vec<String>> {
        let conn = self.connection(DbusBus::Session)?;
        list_mpris_names_blocking(&conn)
    }

    /// Reads snapshots for all active MPRIS media players on the session bus, sorted with
    /// `Playing` players first.
    pub fn mpris_snapshots(&self) -> Result<Vec<MprisSnapshot>> {
        let conn = self.connection(DbusBus::Session)?;
        let names = list_mpris_names_blocking(&conn)?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            if let Ok(snap) = read_mpris_snapshot_blocking(&conn, &name) {
                out.push(snap);
            }
        }
        sort_mpris_snapshots(&mut out);
        Ok(out)
    }

    /// Reads a typed [`MprisSnapshot`] from `service_name` (e.g. `"org.mpris.MediaPlayer2.spotify"`).
    pub fn mpris_snapshot(&self, service_name: &str) -> Result<MprisSnapshot> {
        let conn = self.connection(DbusBus::Session)?;
        read_mpris_snapshot_blocking(&conn, service_name)
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

    /// Invokes `org.mpris.MediaPlayer2.Player.Play` on `service_name`.
    pub fn mpris_play(&self, service_name: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
            "Play",
        )
    }

    /// Invokes `org.mpris.MediaPlayer2.Player.Pause` on `service_name`.
    pub fn mpris_pause(&self, service_name: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
            "Pause",
        )
    }

    /// Invokes `org.mpris.MediaPlayer2.Player.Stop` on `service_name`.
    pub fn mpris_stop(&self, service_name: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
            "Stop",
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

    /// Seeks relative to the current playback position by `offset_us` microseconds
    /// (`org.mpris.MediaPlayer2.Player.Seek`).
    pub fn mpris_seek(&self, service_name: &str, offset_us: i64) -> Result<()> {
        let conn = self.connection(DbusBus::Session)?;
        conn.call_method(
            Some(service_name),
            MPRIS_OBJECT,
            Some(MPRIS_PLAYER_IFACE),
            "Seek",
            &(offset_us,),
        )
        .map_err(|e| WaywiError::DbusError(format!("Player.Seek failed: {e}")))?;
        Ok(())
    }

    /// Sets the absolute playback position in microseconds for `track_id`
    /// (`org.mpris.MediaPlayer2.Player.SetPosition`).
    pub fn mpris_set_position(
        &self,
        service_name: &str,
        track_id: &str,
        position_us: i64,
    ) -> Result<()> {
        let conn = self.connection(DbusBus::Session)?;
        let obj = ObjectPath::try_from(track_id)
            .map_err(|e| WaywiError::DbusError(format!("invalid MPRIS track_id `{track_id}`: {e}")))?;
        conn.call_method(
            Some(service_name),
            MPRIS_OBJECT,
            Some(MPRIS_PLAYER_IFACE),
            "SetPosition",
            &(obj, position_us),
        )
        .map_err(|e| WaywiError::DbusError(format!("Player.SetPosition failed: {e}")))?;
        Ok(())
    }

    /// Sets the player volume (`0.0..=1.0`, or higher if supported) via
    /// `org.mpris.MediaPlayer2.Player.Volume`.
    pub fn mpris_set_volume(&self, service_name: &str, volume: f64) -> Result<()> {
        self.set_property(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
            "Volume",
            Value::from(volume.max(0.0)),
        )
    }

    /// Sets the `Shuffle` flag on `service_name` (`org.mpris.MediaPlayer2.Player.Shuffle`).
    pub fn mpris_set_shuffle(&self, service_name: &str, shuffle: bool) -> Result<()> {
        self.set_property(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
            "Shuffle",
            Value::from(shuffle),
        )
    }

    /// Sets the repeat / loop mode on `service_name` (`org.mpris.MediaPlayer2.Player.LoopStatus`).
    pub fn mpris_set_loop_status(
        &self,
        service_name: &str,
        loop_status: MprisLoopStatus,
    ) -> Result<()> {
        self.set_property(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_PLAYER_IFACE,
            "LoopStatus",
            Value::from(loop_status.as_str()),
        )
    }

    /// Raises/focuses the media player window (`org.mpris.MediaPlayer2.Raise`).
    pub fn mpris_raise(&self, service_name: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_ROOT_IFACE,
            "Raise",
        )
    }

    /// Requests the media player to quit (`org.mpris.MediaPlayer2.Quit`).
    pub fn mpris_quit(&self, service_name: &str) -> Result<()> {
        self.call_method_unit(
            DbusBus::Session,
            service_name,
            MPRIS_OBJECT,
            MPRIS_ROOT_IFACE,
            "Quit",
        )
    }
}

fn list_mpris_names_blocking(conn: &zbus::blocking::Connection) -> Result<Vec<String>> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn)
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
    let names = dbus
        .list_names()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
    let mut out: Vec<String> = names
        .into_iter()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with(MPRIS_PREFIX))
        .collect();
    out.sort();
    Ok(out)
}

async fn list_mpris_names_async(conn: &zbus::Connection) -> Vec<String> {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else {
        return Vec::new();
    };
    let Ok(names) = dbus.list_names().await else {
        return Vec::new();
    };
    let mut out: Vec<String> = names
        .into_iter()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with(MPRIS_PREFIX))
        .collect();
    out.sort();
    out
}

fn read_mpris_snapshot_blocking(
    conn: &zbus::blocking::Connection,
    service_name: &str,
) -> Result<MprisSnapshot> {
    let player_props = get_all_blocking(conn, service_name, MPRIS_OBJECT, MPRIS_PLAYER_IFACE)?;
    let root_props =
        get_all_blocking(conn, service_name, MPRIS_OBJECT, MPRIS_ROOT_IFACE).unwrap_or_default();
    Ok(parse_mpris_snapshot(service_name, &root_props, &player_props))
}

async fn read_mpris_snapshot_async(
    conn: &zbus::Connection,
    service_name: &str,
) -> Option<MprisSnapshot> {
    let player_props =
        get_all_async(conn, service_name, MPRIS_OBJECT, MPRIS_PLAYER_IFACE).await?;
    let root_props = get_all_async(conn, service_name, MPRIS_OBJECT, MPRIS_ROOT_IFACE)
        .await
        .unwrap_or_default();
    Some(parse_mpris_snapshot(service_name, &root_props, &player_props))
}

pub(crate) fn parse_mpris_snapshot(
    service_name: &str,
    root_props: &HashMap<String, OwnedValue>,
    player_props: &HashMap<String, OwnedValue>,
) -> MprisSnapshot {
    let identity = dict_str(root_props, "Identity").filter(|s| !s.is_empty());
    let desktop_entry = dict_str(root_props, "DesktopEntry").filter(|s| !s.is_empty());
    let can_raise = dict_bool(root_props, "CanRaise").unwrap_or(false);
    let can_quit = dict_bool(root_props, "CanQuit").unwrap_or(false);

    let status = MprisPlaybackStatus::from_str_status(
        dict_str(player_props, "PlaybackStatus")
            .as_deref()
            .unwrap_or("Stopped"),
    );
    let loop_status = MprisLoopStatus::from_str_status(
        dict_str(player_props, "LoopStatus")
            .as_deref()
            .unwrap_or("None"),
    );
    let shuffle = dict_bool(player_props, "Shuffle").unwrap_or(false);
    let volume_percent = dict_f64(player_props, "Volume")
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| (v * 100.0).round().clamp(0.0, 255.0) as u8);
    let position_us = dict_i64(player_props, "Position");

    let can_go_next = dict_bool(player_props, "CanGoNext").unwrap_or(false);
    let can_go_previous = dict_bool(player_props, "CanGoPrevious").unwrap_or(false);
    let can_play = dict_bool(player_props, "CanPlay").unwrap_or(false);
    let can_pause = dict_bool(player_props, "CanPause").unwrap_or(false);
    let can_seek = dict_bool(player_props, "CanSeek").unwrap_or(false);
    let can_control = dict_bool(player_props, "CanControl").unwrap_or(true);

    let mut track_id = None;
    let mut title = None;
    let mut artist = None;
    let mut artists = Vec::new();
    let mut album = None;
    let mut art_url = None;
    let mut length_us = None;

    if let Some(DbusValue::Dict(map)) = player_props.get("Metadata").map(owned_value_to_dbus) {
        track_id = map
            .get("mpris:trackid")
            .and_then(DbusValue::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        title = map
            .get("xesam:title")
            .and_then(DbusValue::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        album = map
            .get("xesam:album")
            .and_then(DbusValue::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        art_url = map
            .get("mpris:artUrl")
            .and_then(DbusValue::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        length_us = map.get("mpris:length").and_then(DbusValue::as_i64);

        if let Some(val) = map.get("xesam:artist") {
            match val {
                DbusValue::Array(arr) => {
                    for item in arr {
                        if let Some(s) = item.as_str().filter(|s| !s.is_empty()) {
                            artists.push(s.to_owned());
                        }
                    }
                }
                DbusValue::String(s) if !s.is_empty() => {
                    artists.push(s.clone());
                }
                _ => {}
            }
        }
        artist = artists.first().cloned();
    }

    MprisSnapshot {
        service_name: service_name.to_owned(),
        identity,
        desktop_entry,
        status,
        loop_status,
        shuffle,
        volume_percent,
        track_id,
        title,
        artist,
        artists,
        album,
        art_url,
        length_us,
        position_us,
        can_go_next,
        can_go_previous,
        can_play,
        can_pause,
        can_seek,
        can_control,
        can_raise,
        can_quit,
    }
}

pub(crate) fn sort_mpris_snapshots(snaps: &mut [MprisSnapshot]) {
    snaps.sort_by(|a, b| {
        let rank = |s: MprisPlaybackStatus| match s {
            MprisPlaybackStatus::Playing => 0u8,
            MprisPlaybackStatus::Paused => 1u8,
            MprisPlaybackStatus::Stopped => 2u8,
        };
        rank(a.status)
            .cmp(&rank(b.status))
            .then_with(|| a.service_name.cmp(&b.service_name))
    });
}

pub(crate) struct MprisWorker {
    stop: AsyncStopSignal,
    join: Option<std::thread::JoinHandle<()>>,
}

impl MprisWorker {
    pub(crate) fn spawn(
        sender: calloop::channel::Sender<MprisEvent>,
        shared: DbusSharedBus,
    ) -> Self {
        let stop = AsyncStopSignal::new();
        let stop_sig = stop.clone();
        let join = std::thread::Builder::new()
            .name("waywi-dbus-mpris".to_owned())
            .spawn(move || {
                let Ok(conn) = connect_shared_bus(DbusBus::Session, &shared) else {
                    return;
                };
                futures_lite::future::block_on(run_mpris_worker(conn, sender, stop_sig));
            })
            .ok();
        Self { stop, join }
    }
}

impl Drop for MprisWorker {
    fn drop(&mut self) {
        self.stop.trigger();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

async fn resolve_name_owner_async(conn: &zbus::Connection, name: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some(FDO_DBUS_IFACE),
            "/org/freedesktop/DBus",
            Some(FDO_DBUS_IFACE),
            "GetNameOwner",
            &(name,),
        )
        .await
        .ok()?;
    reply.body().deserialize::<String>().ok()
}

async fn run_mpris_worker(
    conn: zbus::Connection,
    sender: calloop::channel::Sender<MprisEvent>,
    stop_sig: AsyncStopSignal,
) {
    let mut owners: HashMap<String, String> = HashMap::new();
    let mut snapshots: HashMap<String, MprisSnapshot> = HashMap::new();

    for service_name in list_mpris_names_async(&conn).await {
        if let Some(owner) = resolve_name_owner_async(&conn, &service_name).await {
            owners.insert(service_name.clone(), owner);
        }
        if let Some(snap) = read_mpris_snapshot_async(&conn, &service_name).await {
            snapshots.insert(service_name, snap);
        }
    }

    let mut initial_list: Vec<MprisSnapshot> = snapshots.values().cloned().collect();
    sort_mpris_snapshots(&mut initial_list);
    let _ = sender.send(MprisEvent::PlayersUpdated(initial_list));

    // Scoped match rules so the worker only wakes on MPRIS object signals or bus name ownership changes.
    let Ok(mpris_path_rule) = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(MPRIS_OBJECT)
    else {
        return;
    };
    let Ok(owner_rule) = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(FDO_DBUS_IFACE)
        .and_then(|b| b.interface(FDO_DBUS_IFACE))
        .and_then(|b| b.member("NameOwnerChanged"))
    else {
        return;
    };

    let Ok(mpris_stream) =
        zbus::MessageStream::for_match_rule(mpris_path_rule.build(), &conn, Some(64)).await
    else {
        return;
    };
    let Ok(owner_stream) =
        zbus::MessageStream::for_match_rule(owner_rule.build(), &conn, Some(64)).await
    else {
        return;
    };

    use futures_lite::StreamExt;
    let mut stream = mpris_stream.or(owner_stream);

    while !stop_sig.is_stopped() {
        let next_msg = futures_lite::future::or(async { Some(stream.next().await) }, async {
            stop_sig.wait_stopped().await;
            None
        })
        .await;
        let Some(Some(Ok(msg))) = next_msg else {
            break;
        };

        let header = msg.header();
        let iface = header.interface().map(|i| i.as_str()).unwrap_or("");
        let member = header.member().map(|m| m.as_str()).unwrap_or("");
        let path = header.path().map(|p| p.as_str()).unwrap_or("");

        match (iface, member) {
            (FDO_DBUS_IFACE, "NameOwnerChanged") => {
                let Ok((name, _old_owner, new_owner)) =
                    msg.body().deserialize::<(String, String, String)>()
                else {
                    continue;
                };
                if !name.starts_with(MPRIS_PREFIX) {
                    continue;
                }
                if new_owner.is_empty() {
                    owners.remove(&name);
                    if snapshots.remove(&name).is_some() {
                        let _ = sender.send(MprisEvent::PlayerRemoved {
                            service_name: name,
                        });
                        let mut list: Vec<MprisSnapshot> = snapshots.values().cloned().collect();
                        sort_mpris_snapshots(&mut list);
                        let _ = sender.send(MprisEvent::PlayersUpdated(list));
                    }
                } else {
                    owners.insert(name.clone(), new_owner);
                    if let Some(snap) = read_mpris_snapshot_async(&conn, &name).await {
                        let is_new = !snapshots.contains_key(&name);
                        snapshots.insert(name.clone(), snap.clone());
                        if is_new {
                            let _ = sender.send(MprisEvent::PlayerAdded { service_name: name });
                        }
                        let _ = sender.send(MprisEvent::PlayerChanged(Box::new(snap)));
                        let mut list: Vec<MprisSnapshot> = snapshots.values().cloned().collect();
                        sort_mpris_snapshots(&mut list);
                        let _ = sender.send(MprisEvent::PlayersUpdated(list));
                    }
                }
            }
            (FDO_PROPS_IFACE, "PropertiesChanged") if path == MPRIS_OBJECT => {
                let Ok((changed_iface, _, _)) = msg.body().deserialize::<(
                    String,
                    HashMap<String, OwnedValue>,
                    Vec<String>,
                )>() else {
                    continue;
                };
                if changed_iface != MPRIS_PLAYER_IFACE && changed_iface != MPRIS_ROOT_IFACE {
                    continue;
                }
                let msg_sender = header.sender().map(|s| s.as_str()).unwrap_or("");
                let matched_services = match_mpris_sender(&owners, msg_sender);
                let mut any_updated = false;
                for svc in matched_services {
                    if let Some(snap) = read_mpris_snapshot_async(&conn, &svc).await {
                        let is_new = !snapshots.contains_key(&svc);
                        if is_new || snapshots.get(&svc) != Some(&snap) {
                            snapshots.insert(svc.clone(), snap.clone());
                            if is_new {
                                let _ = sender.send(MprisEvent::PlayerAdded { service_name: svc });
                            }
                            let _ = sender.send(MprisEvent::PlayerChanged(Box::new(snap)));
                            any_updated = true;
                        }
                    }
                }
                if any_updated {
                    let mut list: Vec<MprisSnapshot> = snapshots.values().cloned().collect();
                    sort_mpris_snapshots(&mut list);
                    let _ = sender.send(MprisEvent::PlayersUpdated(list));
                }
            }
            (MPRIS_PLAYER_IFACE, "Seeked") if path == MPRIS_OBJECT => {
                let Ok(position_us) = msg.body().deserialize::<i64>() else {
                    continue;
                };
                let msg_sender = header.sender().map(|s| s.as_str()).unwrap_or("");
                let mut any_updated = false;
                for svc in match_mpris_sender(&owners, msg_sender) {
                    let updated_snap = if let Some(entry) = snapshots.get_mut(&svc) {
                        entry.position_us = Some(position_us);
                        any_updated = true;
                        Some(entry.clone())
                    } else {
                        None
                    };
                    let _ = sender.send(MprisEvent::Seeked {
                        service_name: svc,
                        position_us,
                    });
                    if let Some(snap) = updated_snap {
                        let _ = sender.send(MprisEvent::PlayerChanged(Box::new(snap)));
                    }
                }
                if any_updated {
                    let mut list: Vec<MprisSnapshot> = snapshots.values().cloned().collect();
                    sort_mpris_snapshots(&mut list);
                    let _ = sender.send(MprisEvent::PlayersUpdated(list));
                }
            }
            _ => {}
        }
    }
}

fn match_mpris_sender(owners: &HashMap<String, String>, sender: &str) -> Vec<String> {
    if sender.is_empty() {
        return Vec::new();
    }
    let mut matched = HashSet::new();
    if sender.starts_with(MPRIS_PREFIX) {
        matched.insert(sender.to_owned());
    }
    for (svc, unique) in owners {
        if unique == sender || svc == sender {
            matched.insert(svc.clone());
        }
    }
    matched.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::ObjectPath;

    #[test]
    fn parses_mpris_snapshot_with_full_metadata_and_capabilities() {
        let mut root_props: HashMap<String, OwnedValue> = HashMap::new();
        root_props.insert(
            "Identity".into(),
            OwnedValue::try_from(Value::from("Spotify")).unwrap(),
        );
        root_props.insert(
            "DesktopEntry".into(),
            OwnedValue::try_from(Value::from("spotify")).unwrap(),
        );
        root_props.insert(
            "CanRaise".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );

        let mut meta: HashMap<&str, Value<'_>> = HashMap::new();
        meta.insert(
            "mpris:trackid",
            Value::from(ObjectPath::try_from("/com/spotify/track/42").unwrap()),
        );
        meta.insert("xesam:title", Value::from("FREYJA.sys"));
        meta.insert("xesam:album", Value::from("kaoling"));
        meta.insert("xesam:artist", Value::from(vec!["CUL", "GUMI"]));
        meta.insert(
            "mpris:artUrl",
            Value::from("https://example.com/cover.png"),
        );
        meta.insert("mpris:length", Value::from(245_000_000i64));

        let mut player_props: HashMap<String, OwnedValue> = HashMap::new();
        player_props.insert(
            "PlaybackStatus".into(),
            OwnedValue::try_from(Value::from("Playing")).unwrap(),
        );
        player_props.insert(
            "LoopStatus".into(),
            OwnedValue::try_from(Value::from("Track")).unwrap(),
        );
        player_props.insert(
            "Shuffle".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        player_props.insert(
            "Volume".into(),
            OwnedValue::try_from(Value::from(0.85f64)).unwrap(),
        );
        player_props.insert(
            "Position".into(),
            OwnedValue::try_from(Value::from(60_000_000i64)).unwrap(),
        );
        player_props.insert(
            "CanGoNext".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        player_props.insert(
            "CanGoPrevious".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        player_props.insert(
            "CanPlay".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        player_props.insert(
            "CanPause".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        player_props.insert(
            "CanSeek".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        player_props.insert(
            "Metadata".into(),
            OwnedValue::try_from(Value::from(meta)).unwrap(),
        );

        let snap = parse_mpris_snapshot(
            "org.mpris.MediaPlayer2.spotify",
            &root_props,
            &player_props,
        );
        assert_eq!(snap.service_name, "org.mpris.MediaPlayer2.spotify");
        assert_eq!(snap.identity.as_deref(), Some("Spotify"));
        assert_eq!(snap.desktop_entry.as_deref(), Some("spotify"));
        assert_eq!(snap.status, MprisPlaybackStatus::Playing);
        assert_eq!(snap.loop_status, MprisLoopStatus::Track);
        assert!(snap.shuffle);
        assert_eq!(snap.volume_percent, Some(85));
        assert_eq!(snap.track_id.as_deref(), Some("/com/spotify/track/42"));
        assert_eq!(snap.title.as_deref(), Some("FREYJA.sys"));
        assert_eq!(snap.album.as_deref(), Some("kaoling"));
        assert_eq!(snap.artist.as_deref(), Some("CUL"));
        assert_eq!(snap.artists, vec!["CUL".to_owned(), "GUMI".to_owned()]);
        assert_eq!(
            snap.art_url.as_deref(),
            Some("https://example.com/cover.png")
        );
        assert_eq!(snap.length_us, Some(245_000_000));
        assert_eq!(snap.position_us, Some(60_000_000));
        assert!(snap.can_go_next);
        assert!(snap.can_go_previous);
        assert!(snap.can_play);
        assert!(snap.can_pause);
        assert!(snap.can_seek);
        assert!(snap.can_raise);
    }

    #[test]
    fn sorts_mpris_snapshots_playing_first() {
        let mut snaps = vec![
            MprisSnapshot {
                service_name: "org.mpris.MediaPlayer2.firefox".into(),
                status: MprisPlaybackStatus::Paused,
                ..Default::default()
            },
            MprisSnapshot {
                service_name: "org.mpris.MediaPlayer2.mpv".into(),
                status: MprisPlaybackStatus::Stopped,
                ..Default::default()
            },
            MprisSnapshot {
                service_name: "org.mpris.MediaPlayer2.spotify".into(),
                status: MprisPlaybackStatus::Playing,
                ..Default::default()
            },
        ];
        sort_mpris_snapshots(&mut snaps);
        assert_eq!(snaps[0].service_name, "org.mpris.MediaPlayer2.spotify");
        assert_eq!(snaps[1].service_name, "org.mpris.MediaPlayer2.firefox");
        assert_eq!(snaps[2].service_name, "org.mpris.MediaPlayer2.mpv");
    }
}
