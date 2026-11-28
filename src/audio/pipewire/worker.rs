use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pipewire as pw;
use pw::channel as pw_channel;
use pw::device::{Device, DeviceChangeMask};
use pw::metadata::Metadata;
use pw::node::{Node, NodeState};
use pw::proxy::{Listener, ProxyListener, ProxyT};
use pw::spa;
use pw::types::ObjectType;
use spa::param::{ParamInfoFlags, ParamType};
use spa::pod::Pod;
use spa::sys as spa_sys;

use crate::audio::backend::{AudioCommand, SharedAudioState, StreamMetadata, update_snapshot};
use crate::audio::types::{
    AudioCard, AudioChannelPosition, AudioDevice, AudioDirection, AudioEvent, AudioPort,
    AudioPortAvailability, AudioPortTarget, AudioProfile, AudioSnapshot, AudioStream, AudioTarget,
    AudioVolume, apply_linear_balance_with_map, sanitize_linear_channels, scale_linear_channels,
};

use super::pod::{
    PropsExtracted, RouteParams, build_mute_pod, build_profile_pod, build_route_mute_pod,
    build_route_select_pod, build_route_volume_pod_channels, build_volume_pod_channels,
    escape_json, linear_to_raw, parse_default_node_name, parse_profile_pod, parse_props_pod,
    parse_route_pod, raw_to_db, raw_to_linear,
};

#[inline]
const fn to_spa_direction(dir: AudioDirection) -> u32 {
    match dir {
        AudioDirection::Output => spa_sys::SPA_DIRECTION_OUTPUT,
        AudioDirection::Input => spa_sys::SPA_DIRECTION_INPUT,
    }
}

#[derive(Debug, Clone)]
struct AudioNodeRecord {
    id: u32,
    name: String,
    description: String,
    direction: AudioDirection,
    device_id: Option<u32>,
    card_profile_device: Option<i32>,
    channels: usize,
    raw_volume: f32,
    raw_channels: Vec<f32>,
    channel_map: Vec<AudioChannelPosition>,
    nonzero_linear_template: Vec<f32>,
    muted: bool,
}

impl AudioNodeRecord {
    fn new(
        id: u32,
        name: String,
        description: String,
        direction: AudioDirection,
        device_id: Option<u32>,
        card_profile_device: Option<i32>,
    ) -> Self {
        Self {
            id,
            name,
            description,
            direction,
            device_id,
            card_profile_device,
            channels: 2,
            raw_volume: 1.0,
            raw_channels: Vec::new(),
            channel_map: Vec::new(),
            nonzero_linear_template: Vec::new(),
            muted: false,
        }
    }

    #[inline]
    const fn spa_direction(&self) -> u32 {
        to_spa_direction(self.direction)
    }

    fn effective_channel_map(&self) -> Vec<AudioChannelPosition> {
        let count = self.channels.max(1);
        if self.channel_map.len() == count {
            self.channel_map.clone()
        } else {
            AudioChannelPosition::default_map_for_count(count)
        }
    }

    fn linear_channels(&self) -> Vec<f32> {
        if self.raw_channels.is_empty() {
            vec![raw_to_linear(self.raw_volume); self.channels.max(1)]
        } else {
            self.raw_channels
                .iter()
                .copied()
                .map(raw_to_linear)
                .collect()
        }
    }

    fn to_audio_volume(&self) -> AudioVolume {
        AudioVolume {
            linear: raw_to_linear(self.raw_volume),
            channels: self.linear_channels(),
            channel_map: self.effective_channel_map(),
            db: raw_to_db(self.raw_volume),
            muted: self.muted,
        }
    }

    fn set_raw_channel_volumes(&mut self, vols: Vec<f32>) -> bool {
        if vols.is_empty() {
            return false;
        }
        let peak = vols
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .fold(0.0_f32, f32::max);
        let mut changed = false;
        if self.channels != vols.len() {
            self.channels = vols.len();
            changed = true;
        }
        if self.raw_volume != peak {
            self.raw_volume = peak;
            changed = true;
        }
        if self.raw_channels != vols {
            self.raw_channels = vols;
            changed = true;
        }
        if peak > 1e-6 {
            self.nonzero_linear_template = self
                .raw_channels
                .iter()
                .copied()
                .map(raw_to_linear)
                .collect();
        }
        changed
    }

    fn apply_route(&mut self, route: &DeviceRouteRecord) -> bool {
        let mut updated = false;
        if let Some(ref ch_vols) = route.channel_volumes {
            updated |= self.set_raw_channel_volumes(ch_vols.clone());
        } else {
            if let Some(ch) = route.channels
                && self.channels != ch
            {
                self.channels = ch;
                updated = true;
            }
            if let Some(vol) = route.volume
                && self.raw_volume != vol
            {
                self.raw_volume = vol;
                updated = true;
            }
        }
        if let Some(ref cmap) = route.channel_map
            && !cmap.is_empty()
            && self.channel_map != *cmap
        {
            self.channel_map.clone_from(cmap);
            updated = true;
        }
        if let Some(m) = route.muted
            && self.muted != m
        {
            self.muted = m;
            updated = true;
        }
        updated
    }
}

/// Classifies a PipeWire `media.class` value into a stream direction; only audio streams,
/// matching either exact nodes or suffixed subclasses, are accepted.
fn classify_media_class(class: &str) -> Option<AudioDirection> {
    if class.starts_with("Stream/Output/Audio") {
        Some(AudioDirection::Output)
    } else if class.starts_with("Stream/Input/Audio") {
        Some(AudioDirection::Input)
    } else {
        None
    }
}

/// Per-application stream node (`Stream/*/Audio`); volume and mute live on the node Props.
#[derive(Debug, Clone)]
struct AudioStreamRecord {
    id: u32,
    direction: AudioDirection,
    name: String,
    app_name: Option<String>,
    binary_name: Option<String>,
    media_role: Option<String>,
    target_id: Option<u32>,
    corked: bool,
    channels: usize,
    raw_volume: f32,
    raw_channels: Vec<f32>,
    channel_map: Vec<AudioChannelPosition>,
    nonzero_linear_template: Vec<f32>,
    muted: bool,
}

impl AudioStreamRecord {
    fn effective_channel_map(&self) -> Vec<AudioChannelPosition> {
        let count = self.channels.max(1);
        if self.channel_map.len() == count {
            self.channel_map.clone()
        } else {
            AudioChannelPosition::default_map_for_count(count)
        }
    }

    fn linear_channels(&self) -> Vec<f32> {
        if self.raw_channels.is_empty() {
            vec![raw_to_linear(self.raw_volume); self.channels.max(1)]
        } else {
            self.raw_channels
                .iter()
                .copied()
                .map(raw_to_linear)
                .collect()
        }
    }

    fn to_audio_stream(&self) -> AudioStream {
        AudioStream {
            id: self.id,
            direction: self.direction,
            name: self.name.clone(),
            app_name: self.app_name.clone(),
            binary_name: self.binary_name.clone(),
            media_role: self.media_role.clone(),
            device_id: self.target_id,
            corked: self.corked,
            volume: AudioVolume {
                linear: raw_to_linear(self.raw_volume),
                channels: self.linear_channels(),
                channel_map: self.effective_channel_map(),
                db: raw_to_db(self.raw_volume),
                muted: self.muted,
            },
        }
    }

    fn set_raw_channel_volumes(&mut self, ch_vols: Vec<f32>) {
        if ch_vols.is_empty() {
            return;
        }
        self.channels = ch_vols.len().max(1);
        let peak = ch_vols
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .fold(0.0_f32, f32::max);
        self.raw_volume = peak;
        self.raw_channels = ch_vols;
        if peak > 1e-6 {
            self.nonzero_linear_template = self
                .raw_channels
                .iter()
                .copied()
                .map(raw_to_linear)
                .collect();
        }
    }

    fn apply_props(&mut self, props: &PropsExtracted) {
        if let Some(ref ch_vols) = props.channel_volumes {
            self.set_raw_channel_volumes(ch_vols.clone());
        } else {
            if let Some(ch) = props.channels {
                self.channels = ch.max(1);
            }
            if let Some(vol) = props.volume {
                self.raw_volume = vol;
            }
        }
        if let Some(ref cmap) = props.channel_map
            && !cmap.is_empty()
        {
            self.channel_map.clone_from(cmap);
        }
        if let Some(m) = props.muted {
            self.muted = m;
        }
    }
}

#[derive(Debug, Clone)]
struct DeviceRouteRecord {
    index: i32,
    device: i32,
    name: Option<String>,
    channels: Option<usize>,
    volume: Option<f32>,
    channel_volumes: Option<Vec<f32>>,
    channel_map: Option<Vec<AudioChannelPosition>>,
    muted: Option<bool>,
}

#[derive(Debug, Clone)]
struct EnumRouteRecord {
    index: i32,
    direction: u32,
    name: String,
    description: String,
    priority: u32,
    availability: AudioPortAvailability,
    profiles: Vec<i32>,
    devices: Vec<i32>,
}

#[derive(Debug, Clone, Default)]
struct DeviceCardRecord {
    id: u32,
    name: String,
    description: String,
    active_profile_index: Option<i32>,
    active_profile_name: Option<String>,
    profiles: BTreeMap<i32, AudioProfile>,
    enum_routes: BTreeMap<i32, EnumRouteRecord>,
    enum_route_flags: Option<ParamInfoFlags>,
    enum_route_last_seq: Option<i32>,
    profile_flags: Option<ParamInfoFlags>,
    enum_profile_flags: Option<ParamInfoFlags>,
    enum_profile_last_seq: Option<i32>,
}

#[derive(Default)]
struct WorkerState {
    sinks: BTreeMap<u32, AudioNodeRecord>,
    sources: BTreeMap<u32, AudioNodeRecord>,
    streams: BTreeMap<u32, AudioStreamRecord>,
    default_sink_name: Option<String>,
    configured_sink_name: Option<String>,
    default_source_name: Option<String>,
    configured_source_name: Option<String>,
    last_snapshot: Option<Arc<AudioSnapshot>>,
    nodes: HashMap<u32, Node>,
    devices: HashMap<u32, Rc<Device>>,
    device_cards: BTreeMap<u32, DeviceCardRecord>,
    device_routes: HashMap<(u32, u32, i32), DeviceRouteRecord>,
    device_route_flags: HashMap<u32, Option<ParamInfoFlags>>,
    device_route_last_seq: HashMap<u32, i32>,
    listeners: HashMap<u32, Vec<Box<dyn Listener>>>,
    default_metadata: Option<(u32, Metadata)>,
}

fn select_target_node<'a>(
    nodes: &'a BTreeMap<u32, AudioNodeRecord>,
    default_name: Option<&str>,
    configured_name: Option<&str>,
) -> Option<&'a AudioNodeRecord> {
    default_name
        .and_then(|name| nodes.values().find(|s| s.name == name))
        .or_else(|| configured_name.and_then(|name| nodes.values().find(|s| s.name == name)))
        .or_else(|| nodes.values().next())
}

impl WorkerState {
    fn node_map(&self, dir: AudioDirection) -> &BTreeMap<u32, AudioNodeRecord> {
        match dir {
            AudioDirection::Output => &self.sinks,
            AudioDirection::Input => &self.sources,
        }
    }

    fn node_map_mut(&mut self, dir: AudioDirection) -> &mut BTreeMap<u32, AudioNodeRecord> {
        match dir {
            AudioDirection::Output => &mut self.sinks,
            AudioDirection::Input => &mut self.sources,
        }
    }

    fn build_audio_device(&self, rec: &AudioNodeRecord, is_default: bool) -> AudioDevice {
        let active_port = self
            .find_route_for_node(rec)
            .and_then(|r| r.name.clone())
            .or_else(|| {
                let dev_id = rec.device_id?;
                let card = self.device_cards.get(&dev_id)?;
                let route = self.find_route_for_node(rec)?;
                card.enum_routes.get(&route.index).map(|er| er.name.clone())
            });
        let mut ports = Vec::new();
        if let Some(dev_id) = rec.device_id
            && let Some(card) = self.device_cards.get(&dev_id)
        {
            let spa_dir = rec.spa_direction();
            for er in card.enum_routes.values() {
                if er.direction != spa_dir {
                    continue;
                }
                if let Some(card_dev) = rec.card_profile_device
                    && !er.devices.is_empty()
                    && !er.devices.contains(&card_dev)
                {
                    continue;
                }
                if let Some(prof_idx) = card.active_profile_index
                    && !er.profiles.is_empty()
                    && !er.profiles.contains(&prof_idx)
                {
                    continue;
                }
                ports.push(AudioPort {
                    index: er.index,
                    name: er.name.clone(),
                    description: er.description.clone(),
                    priority: er.priority,
                    availability: er.availability,
                });
            }
            ports.sort_by(|a, b| {
                b.priority
                    .cmp(&a.priority)
                    .then_with(|| a.index.cmp(&b.index))
            });
        }
        AudioDevice {
            id: rec.id,
            name: rec.name.clone(),
            description: rec.description.clone(),
            is_default,
            volume: rec.to_audio_volume(),
            active_port,
            ports,
        }
    }

    fn card_list(&self) -> Vec<AudioCard> {
        self.device_cards
            .values()
            .map(|card| {
                let mut profiles: Vec<AudioProfile> = card.profiles.values().cloned().collect();
                profiles.sort_by(|a, b| {
                    b.priority
                        .cmp(&a.priority)
                        .then_with(|| a.index.cmp(&b.index))
                });
                AudioCard {
                    id: card.id,
                    name: card.name.clone(),
                    description: card.description.clone(),
                    active_profile: card.active_profile_name.clone(),
                    profiles,
                }
            })
            .collect()
    }

    fn compute_snapshot(&self) -> AudioSnapshot {
        let default_sink_id = select_target_node(
            &self.sinks,
            self.default_sink_name.as_deref(),
            self.configured_sink_name.as_deref(),
        )
        .map(|n| n.id);
        let default_source_id = select_target_node(
            &self.sources,
            self.default_source_name.as_deref(),
            self.configured_source_name.as_deref(),
        )
        .map(|n| n.id);

        let sinks = self
            .sinks
            .values()
            .map(|rec| self.build_audio_device(rec, Some(rec.id) == default_sink_id))
            .collect();
        let sources = self
            .sources
            .values()
            .map(|rec| self.build_audio_device(rec, Some(rec.id) == default_source_id))
            .collect();
        let streams = self
            .streams
            .values()
            .map(AudioStreamRecord::to_audio_stream)
            .collect();
        let cards = self.card_list();

        AudioSnapshot {
            sinks,
            sources,
            default_sink_id,
            default_source_id,
            streams,
            cards,
        }
    }

    fn publish_if_changed(&mut self, shared: &Arc<Mutex<SharedAudioState>>) {
        let next = self.compute_snapshot();
        update_snapshot(shared, &mut self.last_snapshot, next);
    }

    fn target_node(&self, dir: AudioDirection) -> Option<&AudioNodeRecord> {
        match dir {
            AudioDirection::Output => select_target_node(
                &self.sinks,
                self.default_sink_name.as_deref(),
                self.configured_sink_name.as_deref(),
            ),
            AudioDirection::Input => select_target_node(
                &self.sources,
                self.default_source_name.as_deref(),
                self.configured_source_name.as_deref(),
            ),
        }
    }

    fn resolve_node(&self, dir: AudioDirection, node_id: Option<u32>) -> Option<&AudioNodeRecord> {
        match node_id {
            Some(id) => self.node_map(dir).get(&id),
            None => self.target_node(dir),
        }
    }

    fn update_default_metadata(&mut self, key: &str, value: Option<&str>) -> bool {
        let parsed = value.and_then(parse_default_node_name);
        match key {
            "default.audio.sink" => {
                if self.default_sink_name == parsed {
                    return false;
                }
                self.default_sink_name = parsed;
                true
            }
            "default.configured.audio.sink" => {
                if self.configured_sink_name == parsed {
                    return false;
                }
                self.configured_sink_name = parsed;
                true
            }
            "default.audio.source" => {
                if self.default_source_name == parsed {
                    return false;
                }
                self.default_source_name = parsed;
                true
            }
            "default.configured.audio.source" => {
                if self.configured_source_name == parsed {
                    return false;
                }
                self.configured_source_name = parsed;
                true
            }
            _ => false,
        }
    }

    fn on_device_route_params_info(
        &mut self,
        dev_id: u32,
        route_flags: Option<ParamInfoFlags>,
    ) -> bool {
        let prev = self.device_route_flags.insert(dev_id, route_flags);
        let readable = route_flags.is_some_and(|flags| flags.contains(ParamInfoFlags::READ));
        if !readable || prev != Some(route_flags) {
            self.device_routes.retain(|&(d, _, _), _| d != dev_id);
            self.device_route_last_seq.remove(&dev_id);
        }
        readable
    }

    fn is_device_route_readable(&self, dev_id: u32) -> bool {
        self.device_route_flags
            .get(&dev_id)
            .copied()
            .flatten()
            .is_some_and(|flags| flags.contains(ParamInfoFlags::READ))
    }

    fn record_device_route(
        &mut self,
        dev_id: u32,
        seq: i32,
        direction: u32,
        route: DeviceRouteRecord,
    ) -> bool {
        if !self.is_device_route_readable(dev_id) {
            return false;
        }
        if seq != 0 && self.device_route_last_seq.get(&dev_id).copied() != Some(seq) {
            self.device_routes.retain(|&(d, _, _), _| d != dev_id);
            self.device_route_last_seq.insert(dev_id, seq);
        }
        self.device_routes
            .insert((dev_id, direction, route.device), route);
        true
    }

    fn on_device_card_params_info(
        &mut self,
        dev_id: u32,
        enum_route_flags: Option<ParamInfoFlags>,
        profile_flags: Option<ParamInfoFlags>,
        enum_profile_flags: Option<ParamInfoFlags>,
    ) -> (bool, bool, bool) {
        let Some(card) = self.device_cards.get_mut(&dev_id) else {
            return (false, false, false);
        };
        let er_readable = enum_route_flags.is_some_and(|f| f.contains(ParamInfoFlags::READ));
        if !er_readable {
            card.enum_routes.clear();
            card.enum_route_last_seq = None;
        }
        card.enum_route_flags = enum_route_flags;

        let prof_readable = profile_flags.is_some_and(|f| f.contains(ParamInfoFlags::READ));
        if !prof_readable {
            card.active_profile_index = None;
            card.active_profile_name = None;
        }
        card.profile_flags = profile_flags;

        let ep_readable = enum_profile_flags.is_some_and(|f| f.contains(ParamInfoFlags::READ));
        if !ep_readable {
            card.profiles.clear();
            card.enum_profile_last_seq = None;
        }
        card.enum_profile_flags = enum_profile_flags;

        (er_readable, prof_readable, ep_readable)
    }

    fn record_device_enum_route(&mut self, dev_id: u32, seq: i32, route: RouteParams) -> bool {
        let Some(card) = self.device_cards.get_mut(&dev_id) else {
            return false;
        };
        if !card
            .enum_route_flags
            .is_some_and(|f| f.contains(ParamInfoFlags::READ))
        {
            return false;
        }
        let Some(direction) = route.direction else {
            return false;
        };
        let Some(name) = route.name else {
            return false;
        };
        let description = route.description.unwrap_or_else(|| name.clone());
        if seq != 0 && card.enum_route_last_seq != Some(seq) {
            card.enum_routes.clear();
            card.enum_route_last_seq = Some(seq);
        }
        card.enum_routes.insert(
            route.index,
            EnumRouteRecord {
                index: route.index,
                direction,
                name,
                description,
                priority: route.priority,
                availability: route.availability,
                profiles: route.profiles,
                devices: route.devices,
            },
        );
        true
    }

    fn record_device_profile(&mut self, dev_id: u32, prof: super::pod::ProfileParams) -> bool {
        let Some(card) = self.device_cards.get_mut(&dev_id) else {
            return false;
        };
        if !card
            .profile_flags
            .is_some_and(|f| f.contains(ParamInfoFlags::READ))
        {
            return false;
        }
        let changed = card.active_profile_index != Some(prof.index)
            || card.active_profile_name.as_deref() != Some(prof.name.as_str());
        card.active_profile_index = Some(prof.index);
        card.active_profile_name = Some(prof.name);
        changed
    }

    fn record_device_enum_profile(
        &mut self,
        dev_id: u32,
        seq: i32,
        prof: super::pod::ProfileParams,
    ) -> bool {
        let Some(card) = self.device_cards.get_mut(&dev_id) else {
            return false;
        };
        if !card
            .enum_profile_flags
            .is_some_and(|f| f.contains(ParamInfoFlags::READ))
        {
            return false;
        }
        if seq != 0 && card.enum_profile_last_seq != Some(seq) {
            card.profiles.clear();
            card.enum_profile_last_seq = Some(seq);
        }
        card.profiles.insert(
            prof.index,
            AudioProfile {
                index: prof.index,
                name: prof.name,
                description: prof.description,
                priority: prof.priority,
                available: prof.available,
            },
        );
        true
    }

    fn remove_device_state(&mut self, id: u32) -> bool {
        self.devices.remove(&id);
        let removed_card = self.device_cards.remove(&id).is_some();
        self.device_routes.retain(|&(dev_id, _, _), _| dev_id != id);
        self.device_route_flags.remove(&id);
        self.device_route_last_seq.remove(&id);
        removed_card
    }

    fn find_route(
        &self,
        device_id: Option<u32>,
        direction: u32,
        card_profile_device: Option<i32>,
    ) -> Option<&DeviceRouteRecord> {
        let dev_obj_id = device_id?;
        if let Some(card_dev) = card_profile_device {
            return self.device_routes.get(&(dev_obj_id, direction, card_dev));
        }
        let mut matches = self
            .device_routes
            .iter()
            .filter(|&(&(d, dir, _), _)| d == dev_obj_id && dir == direction)
            .map(|(_, route)| route);
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first)
    }

    fn find_route_for_node(&self, rec: &AudioNodeRecord) -> Option<&DeviceRouteRecord> {
        self.find_route(rec.device_id, rec.spa_direction(), rec.card_profile_device)
    }

    fn find_route_key_for_node(&self, rec: &AudioNodeRecord) -> Option<(u32, u32, i32)> {
        let dev_obj_id = rec.device_id?;
        let dir = rec.spa_direction();
        if let Some(card_dev) = rec.card_profile_device {
            let key = (dev_obj_id, dir, card_dev);
            return self.device_routes.contains_key(&key).then_some(key);
        }
        let mut matches = self
            .device_routes
            .keys()
            .copied()
            .filter(|&(d, dr, _)| d == dev_obj_id && dr == dir);
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first)
    }

    fn apply_node_props(&mut self, dir: AudioDirection, id: u32, props: PropsExtracted) -> bool {
        let (has_route_ch, has_route_vol, has_route_map, has_route_mute) = {
            if let Some(rec) = self.node_map(dir).get(&id)
                && let Some(r) = self.find_route_for_node(rec)
            {
                (
                    r.channels.is_some() || r.channel_volumes.is_some(),
                    r.volume.is_some() || r.channel_volumes.is_some(),
                    r.channel_map.as_ref().is_some_and(|m| !m.is_empty()),
                    r.muted.is_some(),
                )
            } else {
                (false, false, false, false)
            }
        };
        let Some(rec) = self.node_map_mut(dir).get_mut(&id) else {
            return false;
        };
        let mut updated = false;
        if !has_route_vol && let Some(ch_vols) = props.channel_volumes {
            updated |= rec.set_raw_channel_volumes(ch_vols);
        } else {
            if !has_route_ch
                && let Some(ch) = props.channels
                && rec.channels != ch
            {
                rec.channels = ch;
                updated = true;
            }
            if !has_route_vol
                && let Some(vol) = props.volume
                && rec.raw_volume != vol
            {
                rec.raw_volume = vol;
                updated = true;
            }
        }
        if !has_route_map
            && let Some(cmap) = props.channel_map
            && !cmap.is_empty()
            && rec.channel_map != cmap
        {
            rec.channel_map = cmap;
            updated = true;
        }
        if !has_route_mute
            && let Some(m) = props.muted
            && rec.muted != m
        {
            rec.muted = m;
            updated = true;
        }
        updated
    }

    fn route_channel_count(rec: &AudioNodeRecord, route: &DeviceRouteRecord) -> usize {
        route.channels.unwrap_or(rec.channels).max(1)
    }

    fn target_channel_volumes_for_level(
        rec: &AudioNodeRecord,
        route: Option<&DeviceRouteRecord>,
        level: f32,
    ) -> Vec<f32> {
        let count = route.map_or_else(
            || rec.channels.max(1),
            |r| Self::route_channel_count(rec, r),
        );
        let current_linear: Vec<f32> = if let Some(r) = route
            && let Some(ref r_vols) = r.channel_volumes
        {
            r_vols.iter().copied().map(raw_to_linear).collect()
        } else if !rec.raw_channels.is_empty() {
            rec.linear_channels()
        } else {
            Vec::new()
        };
        scale_linear_channels(&current_linear, &rec.nonzero_linear_template, count, level)
    }

    fn target_channel_volumes_for_balance(
        rec: &AudioNodeRecord,
        route: Option<&DeviceRouteRecord>,
        balance: f32,
    ) -> Vec<f32> {
        let count = route.map_or_else(
            || rec.channels.max(1),
            |r| Self::route_channel_count(rec, r),
        );
        let current_linear: Vec<f32> = if let Some(r) = route
            && let Some(ref r_vols) = r.channel_volumes
        {
            r_vols.iter().copied().map(raw_to_linear).collect()
        } else if !rec.raw_channels.is_empty() {
            rec.linear_channels()
        } else {
            Vec::new()
        };
        let cmap: &[AudioChannelPosition] = if let Some(r) = route
            && let Some(ref r_map) = r.channel_map
            && !r_map.is_empty()
        {
            r_map
        } else {
            &rec.channel_map
        };
        apply_linear_balance_with_map(
            &current_linear,
            cmap,
            count,
            raw_to_linear(rec.raw_volume),
            balance,
        )
    }

    fn apply_node_channels(
        &mut self,
        dir: AudioDirection,
        node_id: Option<u32>,
        compute: impl FnOnce(&AudioNodeRecord, Option<&DeviceRouteRecord>) -> Option<Vec<f32>>,
    ) {
        let Some(rec) = self.resolve_node(dir, node_id) else {
            return;
        };
        let resolved_id = rec.id;
        let route_key = self.find_route_key_for_node(rec);
        let route = route_key.and_then(|k| self.device_routes.get(&k));
        let Some(ch_vols) = compute(rec, route) else {
            return;
        };

        if let Some(key) = route_key
            && let Some(device) = self.devices.get(&key.0)
            && let Some(route) = self.device_routes.get(&key)
            && let Some(bytes) =
                build_route_volume_pod_channels(route.index, route.device, &ch_vols)
            && let Some(pod) = Pod::from_bytes(&bytes)
        {
            device.set_param(ParamType::Route, 0, pod);
        } else if let Some(node) = self.nodes.get(&resolved_id)
            && let Some(bytes) = build_volume_pod_channels(&ch_vols)
            && let Some(pod) = Pod::from_bytes(&bytes)
        {
            node.set_param(ParamType::Props, 0, pod);
        }

        // Optimistically update local cached volumes so rapid sequential AdjustVolume commands
        // accumulate smoothly before PipeWire echoes the authoritative param update.
        let raw_vols: Vec<f32> = ch_vols.iter().copied().map(linear_to_raw).collect();
        let peak_raw = raw_vols.iter().copied().fold(0.0_f32, f32::max);
        if let Some(key) = route_key
            && let Some(route) = self.device_routes.get_mut(&key)
        {
            route.channels = Some(raw_vols.len());
            route.volume = Some(peak_raw);
            route.channel_volumes = Some(raw_vols.clone());
        }
        if let Some(rec) = self.node_map_mut(dir).get_mut(&resolved_id) {
            let _ = rec.set_raw_channel_volumes(raw_vols);
        }
    }

    fn apply_node_mute(
        &mut self,
        dir: AudioDirection,
        node_id: Option<u32>,
        explicit: Option<bool>,
    ) {
        let Some(rec) = self.resolve_node(dir, node_id) else {
            return;
        };
        let resolved_id = rec.id;
        let muted = explicit.unwrap_or(!rec.muted);
        let route_key = self.find_route_key_for_node(rec);

        if let Some(key) = route_key
            && let Some(device) = self.devices.get(&key.0)
            && let Some(route) = self.device_routes.get(&key)
            && let Some(bytes) = build_route_mute_pod(route.index, route.device, muted)
            && let Some(pod) = Pod::from_bytes(&bytes)
        {
            device.set_param(ParamType::Route, 0, pod);
        } else if let Some(node) = self.nodes.get(&resolved_id)
            && let Some(bytes) = build_mute_pod(muted)
            && let Some(pod) = Pod::from_bytes(&bytes)
        {
            node.set_param(ParamType::Props, 0, pod);
        }

        if let Some(key) = route_key
            && let Some(route) = self.device_routes.get_mut(&key)
        {
            route.muted = Some(muted);
        }
        if let Some(rec) = self.node_map_mut(dir).get_mut(&resolved_id) {
            rec.muted = muted;
        }
    }

    fn apply_stream_channels(
        &mut self,
        id: u32,
        expected_dir: AudioDirection,
        compute: impl FnOnce(&AudioStreamRecord) -> Option<Vec<f32>>,
    ) {
        let Some(rec) = self
            .streams
            .get(&id)
            .filter(|r| r.direction == expected_dir)
        else {
            return;
        };
        let Some(vols) = compute(rec) else {
            return;
        };
        if let Some(bytes) = build_volume_pod_channels(&vols)
            && let Some(pod) = Pod::from_bytes(&bytes)
            && let Some(node) = self.nodes.get(&id)
        {
            node.set_param(ParamType::Props, 0, pod);
        }
        let raw_vols: Vec<f32> = vols.iter().copied().map(linear_to_raw).collect();
        if let Some(rec) = self.streams.get_mut(&id) {
            rec.set_raw_channel_volumes(raw_vols);
        }
    }

    fn apply_stream_mute(&mut self, id: u32, expected_dir: AudioDirection, explicit: Option<bool>) {
        let Some(rec) = self
            .streams
            .get(&id)
            .filter(|r| r.direction == expected_dir)
        else {
            return;
        };
        let muted = explicit.unwrap_or(!rec.muted);
        if let Some(bytes) = build_mute_pod(muted)
            && let Some(pod) = Pod::from_bytes(&bytes)
            && let Some(node) = self.nodes.get(&id)
        {
            node.set_param(ParamType::Props, 0, pod);
        }
        if let Some(rec) = self.streams.get_mut(&id) {
            rec.muted = muted;
        }
    }

    fn apply_target_volume(&mut self, target: AudioTarget, level: f32) {
        match target {
            AudioTarget::DefaultSink => {
                self.apply_node_channels(AudioDirection::Output, None, |rec, route| {
                    Some(Self::target_channel_volumes_for_level(rec, route, level))
                })
            }
            AudioTarget::Sink(id) => {
                self.apply_node_channels(AudioDirection::Output, Some(id), |rec, route| {
                    Some(Self::target_channel_volumes_for_level(rec, route, level))
                })
            }
            AudioTarget::DefaultSource => {
                self.apply_node_channels(AudioDirection::Input, None, |rec, route| {
                    Some(Self::target_channel_volumes_for_level(rec, route, level))
                })
            }
            AudioTarget::Source(id) => {
                self.apply_node_channels(AudioDirection::Input, Some(id), |rec, route| {
                    Some(Self::target_channel_volumes_for_level(rec, route, level))
                })
            }
            AudioTarget::PlaybackStream(id) => {
                self.apply_stream_channels(id, AudioDirection::Output, |rec| {
                    let cur = if rec.raw_channels.is_empty() {
                        Vec::new()
                    } else {
                        rec.linear_channels()
                    };
                    Some(scale_linear_channels(
                        &cur,
                        &rec.nonzero_linear_template,
                        rec.channels,
                        level,
                    ))
                });
            }
            AudioTarget::CaptureStream(id) => {
                self.apply_stream_channels(id, AudioDirection::Input, |rec| {
                    let cur = if rec.raw_channels.is_empty() {
                        Vec::new()
                    } else {
                        rec.linear_channels()
                    };
                    Some(scale_linear_channels(
                        &cur,
                        &rec.nonzero_linear_template,
                        rec.channels,
                        level,
                    ))
                });
            }
        }
    }

    fn apply_target_adjust_volume(&mut self, target: AudioTarget, delta: f32) {
        let current_linear = match target {
            AudioTarget::DefaultSink => self
                .resolve_node(AudioDirection::Output, None)
                .map(|r| raw_to_linear(r.raw_volume)),
            AudioTarget::Sink(id) => self
                .resolve_node(AudioDirection::Output, Some(id))
                .map(|r| raw_to_linear(r.raw_volume)),
            AudioTarget::DefaultSource => self
                .resolve_node(AudioDirection::Input, None)
                .map(|r| raw_to_linear(r.raw_volume)),
            AudioTarget::Source(id) => self
                .resolve_node(AudioDirection::Input, Some(id))
                .map(|r| raw_to_linear(r.raw_volume)),
            AudioTarget::PlaybackStream(id) => self
                .streams
                .get(&id)
                .filter(|r| r.direction == AudioDirection::Output)
                .map(|r| raw_to_linear(r.raw_volume)),
            AudioTarget::CaptureStream(id) => self
                .streams
                .get(&id)
                .filter(|r| r.direction == AudioDirection::Input)
                .map(|r| raw_to_linear(r.raw_volume)),
        };
        if let Some(cur) = current_linear {
            let next = (cur + delta).clamp(0.0, 1.5);
            self.apply_target_volume(target, next);
        }
    }

    fn apply_target_balance(&mut self, target: AudioTarget, balance: f32) {
        match target {
            AudioTarget::DefaultSink => {
                self.apply_node_channels(AudioDirection::Output, None, |rec, route| {
                    Some(Self::target_channel_volumes_for_balance(
                        rec, route, balance,
                    ))
                })
            }
            AudioTarget::Sink(id) => {
                self.apply_node_channels(AudioDirection::Output, Some(id), |rec, route| {
                    Some(Self::target_channel_volumes_for_balance(
                        rec, route, balance,
                    ))
                })
            }
            AudioTarget::DefaultSource => {
                self.apply_node_channels(AudioDirection::Input, None, |rec, route| {
                    Some(Self::target_channel_volumes_for_balance(
                        rec, route, balance,
                    ))
                })
            }
            AudioTarget::Source(id) => {
                self.apply_node_channels(AudioDirection::Input, Some(id), |rec, route| {
                    Some(Self::target_channel_volumes_for_balance(
                        rec, route, balance,
                    ))
                })
            }
            AudioTarget::PlaybackStream(id) => {
                self.apply_stream_channels(id, AudioDirection::Output, |rec| {
                    let cur = if rec.raw_channels.is_empty() {
                        Vec::new()
                    } else {
                        rec.linear_channels()
                    };
                    Some(apply_linear_balance_with_map(
                        &cur,
                        &rec.channel_map,
                        rec.channels,
                        raw_to_linear(rec.raw_volume),
                        balance,
                    ))
                });
            }
            AudioTarget::CaptureStream(id) => {
                self.apply_stream_channels(id, AudioDirection::Input, |rec| {
                    let cur = if rec.raw_channels.is_empty() {
                        Vec::new()
                    } else {
                        rec.linear_channels()
                    };
                    Some(apply_linear_balance_with_map(
                        &cur,
                        &rec.channel_map,
                        rec.channels,
                        raw_to_linear(rec.raw_volume),
                        balance,
                    ))
                });
            }
        }
    }

    fn apply_target_channel_volumes(&mut self, target: AudioTarget, volumes: &[f32]) {
        let Some(clean) = sanitize_linear_channels(volumes) else {
            return;
        };
        match target {
            AudioTarget::DefaultSink => {
                self.apply_node_channels(AudioDirection::Output, None, |_, _| Some(clean));
            }
            AudioTarget::Sink(id) => {
                self.apply_node_channels(AudioDirection::Output, Some(id), |_, _| Some(clean));
            }
            AudioTarget::DefaultSource => {
                self.apply_node_channels(AudioDirection::Input, None, |_, _| Some(clean));
            }
            AudioTarget::Source(id) => {
                self.apply_node_channels(AudioDirection::Input, Some(id), |_, _| Some(clean));
            }
            AudioTarget::PlaybackStream(id) => {
                self.apply_stream_channels(id, AudioDirection::Output, |_| Some(clean));
            }
            AudioTarget::CaptureStream(id) => {
                self.apply_stream_channels(id, AudioDirection::Input, |_| Some(clean));
            }
        }
    }

    fn apply_target_mute(&mut self, target: AudioTarget, explicit: Option<bool>) {
        match target {
            AudioTarget::DefaultSink => {
                self.apply_node_mute(AudioDirection::Output, None, explicit)
            }
            AudioTarget::Sink(id) => {
                self.apply_node_mute(AudioDirection::Output, Some(id), explicit)
            }
            AudioTarget::DefaultSource => {
                self.apply_node_mute(AudioDirection::Input, None, explicit)
            }
            AudioTarget::Source(id) => {
                self.apply_node_mute(AudioDirection::Input, Some(id), explicit)
            }
            AudioTarget::PlaybackStream(id) => {
                self.apply_stream_mute(id, AudioDirection::Output, explicit)
            }
            AudioTarget::CaptureStream(id) => {
                self.apply_stream_mute(id, AudioDirection::Input, explicit)
            }
        }
    }

    fn select_default_node(&self, dir: AudioDirection, id: u32) {
        let meta_key = match dir {
            AudioDirection::Output => "default.configured.audio.sink",
            AudioDirection::Input => "default.configured.audio.source",
        };
        if let Some(rec) = self.node_map(dir).get(&id)
            && let Some((_, meta)) = &self.default_metadata
        {
            let json = format!("{{\"name\":\"{}\"}}", escape_json(&rec.name));
            meta.set_property(0, meta_key, Some("Spa:String:JSON"), Some(&json));
        }
    }

    fn move_stream(
        &mut self,
        dir: AudioDirection,
        stream_id: u32,
        device_id: u32,
        shared: &Arc<Mutex<SharedAudioState>>,
    ) {
        let Some(target_name) = self.node_map(dir).get(&device_id).map(|n| n.name.clone()) else {
            return;
        };
        let Some(stream_rec) = self.streams.get_mut(&stream_id) else {
            return;
        };
        if stream_rec.direction != dir {
            return;
        }
        if let Some((_, meta)) = &self.default_metadata {
            let id_str = device_id.to_string();
            meta.set_property(stream_id, "target.node", Some("Spa:Id"), Some(&id_str));
            meta.set_property(
                stream_id,
                "target.object",
                Some("Spa:String"),
                Some(&target_name),
            );
            stream_rec.target_id = Some(device_id);
            self.publish_if_changed(shared);
        }
    }

    fn resolve_port_selection(
        &self,
        dir: AudioDirection,
        node_id: Option<u32>,
        port_name: &str,
    ) -> Option<(u32, i32, i32)> {
        let rec = self.resolve_node(dir, node_id)?;
        let dev_id = rec.device_id?;
        let card = self.device_cards.get(&dev_id)?;
        let spa_dir = rec.spa_direction();
        let er = card
            .enum_routes
            .values()
            .filter(|er| er.direction == spa_dir && er.name == port_name)
            .find(|er| {
                rec.card_profile_device
                    .is_none_or(|cd| er.devices.is_empty() || er.devices.contains(&cd))
            })
            .or_else(|| {
                card.enum_routes
                    .values()
                    .find(|er| er.direction == spa_dir && er.name == port_name)
            })?;
        let target_card_dev = rec
            .card_profile_device
            .or_else(|| self.find_route_for_node(rec).map(|r| r.device))
            .or_else(|| er.devices.first().copied())?;
        Some((dev_id, er.index, target_card_dev))
    }

    fn select_port(&self, target: AudioPortTarget, port_name: &str) {
        let (dir, node_id) = match target {
            AudioPortTarget::DefaultSink => (AudioDirection::Output, None),
            AudioPortTarget::Sink(id) => (AudioDirection::Output, Some(id)),
            AudioPortTarget::DefaultSource => (AudioDirection::Input, None),
            AudioPortTarget::Source(id) => (AudioDirection::Input, Some(id)),
        };
        let Some((dev_id, route_index, card_dev)) =
            self.resolve_port_selection(dir, node_id, port_name)
        else {
            return;
        };
        let Some(device) = self.devices.get(&dev_id) else {
            return;
        };
        if let Some(bytes) = build_route_select_pod(route_index, card_dev)
            && let Some(pod) = Pod::from_bytes(&bytes)
        {
            device.set_param(ParamType::Route, 0, pod);
        }
    }

    fn resolve_profile_selection(&self, card_id: u32, profile_name: &str) -> Option<i32> {
        let card = self.device_cards.get(&card_id)?;
        card.profiles
            .values()
            .find(|p| p.name == profile_name)
            .map(|p| p.index)
    }

    fn select_card_profile(&self, card_id: u32, profile_name: &str) {
        let Some(prof_index) = self.resolve_profile_selection(card_id, profile_name) else {
            return;
        };
        let Some(device) = self.devices.get(&card_id) else {
            return;
        };
        if let Some(bytes) = build_profile_pod(prof_index)
            && let Some(pod) = Pod::from_bytes(&bytes)
        {
            device.set_param(ParamType::Profile, 0, pod);
        }
    }

    fn insert_stream_record(
        &mut self,
        id: u32,
        direction: AudioDirection,
        meta: StreamMetadata,
        target_id: Option<u32>,
    ) {
        self.streams.insert(
            id,
            AudioStreamRecord {
                id,
                direction,
                name: meta.name,
                app_name: meta.app_name,
                binary_name: meta.binary_name,
                media_role: meta.media_role,
                target_id,
                corked: false,
                channels: 2,
                raw_volume: 1.0,
                raw_channels: Vec::new(),
                channel_map: Vec::new(),
                nonzero_linear_template: Vec::new(),
                muted: false,
            },
        );
    }
}

pub(crate) fn run_worker_loop(
    shared: Arc<Mutex<SharedAudioState>>,
    stop_flag: Arc<AtomicBool>,
    rx_slot: &mut Option<pw_channel::Receiver<AudioCommand>>,
    sender_slot: &Arc<Mutex<Option<pw_channel::Sender<AudioCommand>>>>,
) {
    let mut backoff = Duration::from_millis(500);
    let mut ever_connected = false;
    let mut first_rx = rx_slot.take();

    while !stop_flag.load(Ordering::Acquire) {
        let rx = match first_rx.take() {
            Some(r) => r,
            None => {
                let (new_tx, new_rx) = pw_channel::channel::<AudioCommand>();
                if let Ok(mut guard) = sender_slot.lock() {
                    *guard = Some(new_tx);
                }
                new_rx
            }
        };

        let result = run_single_pw_session(&shared, &stop_flag, rx, &mut ever_connected);
        if stop_flag.load(Ordering::Acquire) {
            break;
        }

        if let Ok(mut guard) = shared.lock()
            && guard.connected
        {
            guard.connected = false;
            guard.push_event(AudioEvent::BackendDisconnected);
        }

        if result.is_ok() {
            backoff = Duration::from_millis(500);
        } else {
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }

        let mut slept = Duration::ZERO;
        while slept < backoff && !stop_flag.load(Ordering::Acquire) {
            let chunk = Duration::from_millis(20).min(backoff - slept);
            thread::sleep(chunk);
            slept += chunk;
        }
    }
}

fn run_single_pw_session(
    shared: &Arc<Mutex<SharedAudioState>>,
    stop_flag: &Arc<AtomicBool>,
    rx: pw_channel::Receiver<AudioCommand>,
    ever_connected: &mut bool,
) -> std::result::Result<(), ()> {
    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(|_| ())?;
    let context = pw::context::ContextRc::new(&main_loop, None).map_err(|_| ())?;
    let core = context.connect_rc(None).map_err(|_| ())?;
    let registry = core.get_registry_rc().map_err(|_| ())?;

    {
        if let Ok(mut guard) = shared.lock() {
            guard.connected = true;
            if *ever_connected {
                guard.push_event(AudioEvent::BackendReconnected);
            }
            *ever_connected = true;
        }
    }

    let state = Rc::new(RefCell::new(WorkerState::default()));

    let main_loop_weak = main_loop.downgrade();
    let stop_check = Arc::clone(stop_flag);
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, _res, _msg| {
            if (id == 0 || stop_check.load(Ordering::Acquire))
                && let Some(ml) = main_loop_weak.upgrade()
            {
                ml.quit();
            }
        })
        .register();

    let state_cmd = Rc::clone(&state);
    let shared_cmd = Arc::clone(shared);
    let main_loop_cmd = main_loop.downgrade();
    let _cmd_rx = rx.attach(main_loop.loop_(), move |cmd| match cmd {
        AudioCommand::SetVolume(target, level) => {
            state_cmd.borrow_mut().apply_target_volume(target, level);
        }
        AudioCommand::AdjustVolume(target, delta) => {
            state_cmd
                .borrow_mut()
                .apply_target_adjust_volume(target, delta);
        }
        AudioCommand::SetBalance(target, balance) => {
            state_cmd.borrow_mut().apply_target_balance(target, balance);
        }
        AudioCommand::SetChannelVolumes(target, vols) => {
            state_cmd
                .borrow_mut()
                .apply_target_channel_volumes(target, &vols);
        }
        AudioCommand::SetMute(target, muted) => {
            state_cmd
                .borrow_mut()
                .apply_target_mute(target, Some(muted));
        }
        AudioCommand::ToggleMute(target) => {
            state_cmd.borrow_mut().apply_target_mute(target, None);
        }
        AudioCommand::SetDefaultDevice(dir, id) => {
            state_cmd.borrow().select_default_node(dir, id);
        }
        AudioCommand::MoveStream(dir, stream_id, device_id) => {
            state_cmd
                .borrow_mut()
                .move_stream(dir, stream_id, device_id, &shared_cmd);
        }
        AudioCommand::SetPort(target, port_name) => {
            state_cmd.borrow().select_port(target, &port_name);
        }
        AudioCommand::SetCardProfile(card_id, profile_name) => {
            state_cmd
                .borrow()
                .select_card_profile(card_id, &profile_name);
        }
        AudioCommand::Shutdown => {
            if let Some(ml) = main_loop_cmd.upgrade() {
                ml.quit();
            }
        }
    });

    let registry_weak = registry.downgrade();
    let state_global = Rc::clone(&state);
    let shared_global = Arc::clone(shared);

    let state_remove = Rc::clone(&state);
    let shared_remove = Arc::clone(shared);

    let _reg_listener = registry
        .add_listener_local()
        .global(move |obj| {
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            let Some(props) = obj.props else {
                return;
            };

            match obj.type_ {
                ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                    let Ok(metadata): std::result::Result<Metadata, _> = registry.bind(obj) else {
                        return;
                    };
                    let state_meta = Rc::clone(&state_global);
                    let shared_meta = Arc::clone(&shared_global);
                    let listener = metadata
                        .add_listener_local()
                        .property(move |_subject, key, _type, value| {
                            if let Some(k) = key {
                                let mut st = state_meta.borrow_mut();
                                if st.update_default_metadata(k, value) {
                                    st.publish_if_changed(&shared_meta);
                                }
                            }
                            0
                        })
                        .register();
                    let obj_id = obj.id;
                    let mut st = state_global.borrow_mut();
                    st.default_metadata = Some((obj_id, metadata));
                    st.listeners
                        .entry(obj_id)
                        .or_default()
                        .push(Box::new(listener));
                }
                ObjectType::Device => {
                    let Ok(device): std::result::Result<Device, _> = registry.bind(obj) else {
                        return;
                    };
                    let id = obj.id;
                    let is_audio_card = props.get("media.class") == Some("Audio/Device");
                    if is_audio_card {
                        let card_name = props.get("device.name").unwrap_or("unknown").to_owned();
                        let card_desc = props
                            .get("device.description")
                            .or_else(|| props.get("device.nick"))
                            .unwrap_or(&card_name)
                            .to_owned();
                        state_global.borrow_mut().device_cards.insert(
                            id,
                            DeviceCardRecord {
                                id,
                                name: card_name,
                                description: card_desc,
                                ..DeviceCardRecord::default()
                            },
                        );
                    }
                    let device = Rc::new(device);
                    let dev_info_weak = Rc::downgrade(&device);
                    let state_info = Rc::clone(&state_global);
                    let state_dev = Rc::clone(&state_global);
                    let shared_dev = Arc::clone(&shared_global);
                    let listener = device
                        .add_listener_local()
                        .info(move |info| {
                            if !info.change_mask().contains(DeviceChangeMask::PARAMS) {
                                return;
                            }
                            let find_flags = |param_type: ParamType| {
                                info.params()
                                    .iter()
                                    .find(|p| p.id() == param_type)
                                    .map(spa::param::ParamInfo::flags)
                            };
                            let route_flags = find_flags(ParamType::Route);
                            let enum_route_flags = find_flags(ParamType::EnumRoute);
                            let profile_flags = find_flags(ParamType::Profile);
                            let enum_profile_flags = find_flags(ParamType::EnumProfile);
                            let (enum_route, (enum_er, enum_prof, enum_ep)) = {
                                let mut st = state_info.borrow_mut();
                                let r = st.on_device_route_params_info(id, route_flags);
                                let c = st.on_device_card_params_info(
                                    id,
                                    enum_route_flags,
                                    profile_flags,
                                    enum_profile_flags,
                                );
                                (r, c)
                            };
                            if let Some(dev) = dev_info_weak.upgrade() {
                                if enum_ep {
                                    dev.enum_params(0, Some(ParamType::EnumProfile), 0, u32::MAX);
                                }
                                if enum_prof {
                                    dev.enum_params(0, Some(ParamType::Profile), 0, u32::MAX);
                                }
                                if enum_er {
                                    dev.enum_params(0, Some(ParamType::EnumRoute), 0, u32::MAX);
                                }
                                if enum_route {
                                    dev.enum_params(0, Some(ParamType::Route), 0, u32::MAX);
                                }
                            }
                        })
                        .param(move |seq, param_id, _index, _next, param| {
                            let Some(pod) = param else {
                                return;
                            };
                            if param_id == ParamType::EnumRoute {
                                let Some(route) = parse_route_pod(pod.as_bytes()) else {
                                    return;
                                };
                                let mut st = state_dev.borrow_mut();
                                if st.record_device_enum_route(id, seq, route) {
                                    st.publish_if_changed(&shared_dev);
                                }
                                return;
                            }
                            if param_id == ParamType::Profile {
                                let Some(prof) = parse_profile_pod(pod.as_bytes()) else {
                                    return;
                                };
                                let mut st = state_dev.borrow_mut();
                                if st.record_device_profile(id, prof) {
                                    st.publish_if_changed(&shared_dev);
                                }
                                return;
                            }
                            if param_id == ParamType::EnumProfile {
                                let Some(prof) = parse_profile_pod(pod.as_bytes()) else {
                                    return;
                                };
                                let mut st = state_dev.borrow_mut();
                                if st.record_device_enum_profile(id, seq, prof) {
                                    st.publish_if_changed(&shared_dev);
                                }
                                return;
                            }
                            if param_id != ParamType::Route {
                                return;
                            }
                            let Some(route) = parse_route_pod(pod.as_bytes()) else {
                                return;
                            };
                            let Some(direction) = route.direction else {
                                return;
                            };
                            if direction != spa_sys::SPA_DIRECTION_OUTPUT
                                && direction != spa_sys::SPA_DIRECTION_INPUT
                            {
                                return;
                            }
                            let Some(route_dev) = route.device else {
                                return;
                            };
                            let mut st = state_dev.borrow_mut();
                            let route_rec = DeviceRouteRecord {
                                index: route.index,
                                device: route_dev,
                                name: route.name,
                                channels: route.channels,
                                volume: route.volume,
                                channel_volumes: route.channel_volumes,
                                channel_map: route.channel_map,
                                muted: route.muted,
                            };
                            if !st.record_device_route(id, seq, direction, route_rec.clone()) {
                                return;
                            }
                            let unique_dir_route = st
                                .device_routes
                                .keys()
                                .filter(|&&(d, dir, _)| d == id && dir == direction)
                                .count()
                                == 1;
                            let target_map = if direction == spa_sys::SPA_DIRECTION_INPUT {
                                &mut st.sources
                            } else {
                                &mut st.sinks
                            };
                            for node_rec in target_map.values_mut() {
                                if node_rec.device_id == Some(id)
                                    && (node_rec.card_profile_device == Some(route_dev)
                                        || (node_rec.card_profile_device.is_none()
                                            && unique_dir_route))
                                {
                                    let _ = node_rec.apply_route(&route_rec);
                                }
                            }
                            st.publish_if_changed(&shared_dev);
                        })
                        .register();

                    let state_removed = Rc::downgrade(&state_global);
                    let proxy_listener: ProxyListener = device
                        .upcast_ref()
                        .add_listener_local()
                        .removed(move || {
                            if let Some(st_rc) = state_removed.upgrade() {
                                let mut st = st_rc.borrow_mut();
                                let _ = st.remove_device_state(id);
                                st.listeners.remove(&id);
                            }
                        })
                        .register();

                    let mut st = state_global.borrow_mut();
                    st.devices.insert(id, device);
                    let entry = st.listeners.entry(id).or_default();
                    entry.push(Box::new(listener));
                    entry.push(Box::new(proxy_listener));
                }
                ObjectType::Node
                    if matches!(
                        props.get("media.class"),
                        Some("Audio/Sink" | "Audio/Source")
                    ) =>
                {
                    let direction = if props.get("media.class") == Some("Audio/Source") {
                        AudioDirection::Input
                    } else {
                        AudioDirection::Output
                    };
                    let Ok(node): std::result::Result<Node, _> = registry.bind(obj) else {
                        return;
                    };
                    let id = obj.id;
                    let name = props.get("node.name").unwrap_or("unknown").to_string();
                    let description = props
                        .get("node.description")
                        .or_else(|| props.get("node.nick"))
                        .unwrap_or(&name)
                        .to_string();
                    let device_id = props.get("device.id").and_then(|s| s.parse::<u32>().ok());
                    let card_profile_device = props
                        .get("card.profile.device")
                        .and_then(|s| s.parse::<i32>().ok());

                    {
                        let mut st = state_global.borrow_mut();
                        let mut rec = AudioNodeRecord::new(
                            id,
                            name,
                            description,
                            direction,
                            device_id,
                            card_profile_device,
                        );
                        if let Some(route) = st.find_route_for_node(&rec).cloned() {
                            rec.apply_route(&route);
                        }
                        st.node_map_mut(direction).insert(id, rec);
                        st.publish_if_changed(&shared_global);
                    }

                    let state_info = Rc::clone(&state_global);
                    let shared_info = Arc::clone(&shared_global);
                    let state_param = Rc::clone(&state_global);
                    let shared_param = Arc::clone(&shared_global);
                    let listener = node
                        .add_listener_local()
                        .info(move |info| {
                            let Some(info_props) = info.props() else {
                                return;
                            };
                            let dev_id = info_props
                                .get("device.id")
                                .and_then(|s| s.parse::<u32>().ok());
                            let card_dev = info_props
                                .get("card.profile.device")
                                .and_then(|s| s.parse::<i32>().ok());
                            if dev_id.is_none() && card_dev.is_none() {
                                return;
                            }
                            let mut st = state_info.borrow_mut();
                            let (eff_dev, eff_dir, eff_card) = {
                                let Some(rec) = st.node_map_mut(direction).get_mut(&id) else {
                                    return;
                                };
                                if dev_id.is_some() {
                                    rec.device_id = dev_id;
                                }
                                if card_dev.is_some() {
                                    rec.card_profile_device = card_dev;
                                }
                                (rec.device_id, rec.spa_direction(), rec.card_profile_device)
                            };
                            if let Some(route) = st.find_route(eff_dev, eff_dir, eff_card).cloned()
                                && let Some(rec) = st.node_map_mut(direction).get_mut(&id)
                            {
                                let _ = rec.apply_route(&route);
                            }
                            st.publish_if_changed(&shared_info);
                        })
                        .param(move |_seq, param_id, _index, _next, param| {
                            if param_id != ParamType::Props {
                                return;
                            }
                            let Some(pod) = param else {
                                return;
                            };
                            if let Some(props_parsed) = parse_props_pod(pod.as_bytes()) {
                                let mut st = state_param.borrow_mut();
                                let _ = st.apply_node_props(direction, id, props_parsed);
                                st.publish_if_changed(&shared_param);
                            }
                        })
                        .register();

                    node.subscribe_params(&[ParamType::Props]);
                    node.enum_params(0, Some(ParamType::Props), 0, u32::MAX);

                    let state_removed = Rc::downgrade(&state_global);
                    let proxy_listener: ProxyListener = node
                        .upcast_ref()
                        .add_listener_local()
                        .removed(move || {
                            if let Some(st_rc) = state_removed.upgrade() {
                                let mut st = st_rc.borrow_mut();
                                st.nodes.remove(&id);
                                st.listeners.remove(&id);
                            }
                        })
                        .register();

                    let mut st = state_global.borrow_mut();
                    st.nodes.insert(id, node);
                    let entry = st.listeners.entry(id).or_default();
                    entry.push(Box::new(listener));
                    entry.push(Box::new(proxy_listener));
                }
                ObjectType::Node
                    if classify_media_class(props.get("media.class").unwrap_or_default())
                        .is_some()
                        && !matches!(
                            props.get("node.name"),
                            Some("waywi-audio" | "waywi-capture")
                        ) =>
                {
                    let Some(direction) =
                        classify_media_class(props.get("media.class").unwrap_or_default())
                    else {
                        return;
                    };
                    let Ok(node): std::result::Result<Node, _> = registry.bind(obj) else {
                        return;
                    };
                    let id = obj.id;
                    let meta = StreamMetadata::extract(
                        Some(props.get("node.name").unwrap_or("stream")),
                        |k| props.get(k),
                    );
                    let target_id = parse_stream_target_id(props);

                    {
                        let mut st = state_global.borrow_mut();
                        st.insert_stream_record(id, direction, meta, target_id);
                        st.publish_if_changed(&shared_global);
                    }

                    let state_info = Rc::clone(&state_global);
                    let shared_info = Arc::clone(&shared_global);
                    let state_param = Rc::clone(&state_global);
                    let shared_param = Arc::clone(&shared_global);
                    let listener = node
                        .add_listener_local()
                        .info(move |info| {
                            let corked =
                                matches!(info.state(), NodeState::Suspended | NodeState::Idle);
                            let mut st = state_info.borrow_mut();
                            let Some(rec) = st.streams.get_mut(&id) else {
                                return;
                            };
                            let mut changed = false;
                            if rec.corked != corked {
                                rec.corked = corked;
                                changed = true;
                            }
                            if let Some(info_props) = info.props() {
                                let next_meta =
                                    StreamMetadata::extract(info_props.get("node.name"), |k| {
                                        info_props.get(k)
                                    });
                                let next_target = parse_stream_target_id(info_props);
                                if next_meta.name != "unknown" && rec.name != next_meta.name {
                                    rec.name = next_meta.name;
                                    changed = true;
                                }
                                if next_meta.app_name.is_some()
                                    && rec.app_name != next_meta.app_name
                                {
                                    rec.app_name = next_meta.app_name;
                                    changed = true;
                                }
                                if next_meta.binary_name.is_some()
                                    && rec.binary_name != next_meta.binary_name
                                {
                                    rec.binary_name = next_meta.binary_name;
                                    changed = true;
                                }
                                if next_meta.media_role.is_some()
                                    && rec.media_role != next_meta.media_role
                                {
                                    rec.media_role = next_meta.media_role;
                                    changed = true;
                                }
                                if next_target.is_some() && rec.target_id != next_target {
                                    rec.target_id = next_target;
                                    changed = true;
                                }
                            }
                            if changed {
                                st.publish_if_changed(&shared_info);
                            }
                        })
                        .param(move |_seq, param_id, _index, _next, param| {
                            if param_id != ParamType::Props {
                                return;
                            }
                            let Some(pod) = param else {
                                return;
                            };
                            if let Some(props_parsed) = parse_props_pod(pod.as_bytes()) {
                                let mut st = state_param.borrow_mut();
                                if let Some(rec) = st.streams.get_mut(&id) {
                                    rec.apply_props(&props_parsed);
                                }
                                st.publish_if_changed(&shared_param);
                            }
                        })
                        .register();

                    node.subscribe_params(&[ParamType::Props]);
                    node.enum_params(0, Some(ParamType::Props), 0, u32::MAX);

                    let state_removed = Rc::downgrade(&state_global);
                    let proxy_listener: ProxyListener = node
                        .upcast_ref()
                        .add_listener_local()
                        .removed(move || {
                            if let Some(st_rc) = state_removed.upgrade() {
                                let mut st = st_rc.borrow_mut();
                                st.nodes.remove(&id);
                                st.listeners.remove(&id);
                            }
                        })
                        .register();

                    let mut st = state_global.borrow_mut();
                    st.nodes.insert(id, node);
                    let entry = st.listeners.entry(id).or_default();
                    entry.push(Box::new(listener));
                    entry.push(Box::new(proxy_listener));
                }
                _ => {}
            }
        })
        .global_remove(move |id| {
            let mut st = state_remove.borrow_mut();
            let removed_sink = st.sinks.remove(&id).is_some();
            let removed_source = st.sources.remove(&id).is_some();
            let removed_stream = st.streams.remove(&id).is_some();
            if st
                .default_metadata
                .as_ref()
                .is_some_and(|(meta_id, _)| *meta_id == id)
            {
                st.default_metadata = None;
            }
            st.nodes.remove(&id);
            let removed_card = st.remove_device_state(id);
            st.listeners.remove(&id);
            if removed_sink || removed_source || removed_stream || removed_card {
                st.publish_if_changed(&shared_remove);
            }
        })
        .register();

    main_loop.run();

    let mut st = state.borrow_mut();
    st.default_metadata = None;
    st.nodes.clear();
    st.devices.clear();
    st.device_cards.clear();
    st.device_routes.clear();
    st.device_route_flags.clear();
    st.device_route_last_seq.clear();
    st.streams.clear();
    st.listeners.clear();

    Ok(())
}

fn parse_stream_target_id(props: &spa::utils::dict::DictRef) -> Option<u32> {
    props
        .get("node.target")
        .or_else(|| props.get("target.node"))
        .or_else(|| props.get("target.object"))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u32>().ok())
}

#[cfg(test)]
mod tests {
    use super::classify_media_class;
    use crate::audio::types::AudioDirection;

    #[test]
    fn classify_media_class_accepts_audio_streams_only() {
        assert_eq!(
            classify_media_class("Stream/Output/Audio"),
            Some(AudioDirection::Output)
        );
        assert_eq!(
            classify_media_class("Stream/Input/Audio"),
            Some(AudioDirection::Input)
        );
    }

    #[test]
    fn classify_media_class_rejects_non_audio_nodes() {
        assert_eq!(classify_media_class("Audio/Sink"), None);
        assert_eq!(classify_media_class("Audio/Source"), None);
        assert_eq!(classify_media_class("Stream/Output/Video"), None);
        assert_eq!(classify_media_class("Stream/Input/Video"), None);
        assert_eq!(classify_media_class(""), None);
    }

    #[test]
    fn device_route_sync_refreshes_on_serial_toggle_and_prunes_stale_routes() {
        use super::{DeviceRouteRecord, WorkerState};
        use pipewire::spa::param::ParamInfoFlags;
        use pipewire::spa::sys as spa_sys;

        let mut st = WorkerState::default();
        let dev_id = 45;
        let out_dir = spa_sys::SPA_DIRECTION_OUTPUT;
        let in_dir = spa_sys::SPA_DIRECTION_INPUT;

        assert!(!st.on_device_route_params_info(49, None));
        assert!(st.on_device_route_params_info(dev_id, Some(ParamInfoFlags::READWRITE)));

        assert!(st.record_device_route(
            dev_id,
            0x4000_000d,
            in_dir,
            DeviceRouteRecord {
                index: 1,
                device: 0,
                name: Some("analog-input-rear-mic".into()),
                channels: Some(2),
                volume: Some(0.02),
                channel_volumes: Some(vec![0.02, 0.02]),
                channel_map: None,
                muted: Some(false),
            },
        ));
        assert!(st.record_device_route(
            dev_id,
            0x4000_000d,
            out_dir,
            DeviceRouteRecord {
                index: 3,
                device: 5,
                name: Some("analog-output-lineout".into()),
                channels: Some(2),
                volume: Some(0.04),
                channel_volumes: Some(vec![0.04, 0.04]),
                channel_map: None,
                muted: Some(false),
            },
        ));
        assert_eq!(st.device_routes[&(dev_id, out_dir, 5)].index, 3);

        assert!(st.on_device_route_params_info(dev_id, Some(ParamInfoFlags::READWRITE)));
        assert_eq!(st.device_routes[&(dev_id, out_dir, 5)].index, 3);

        assert!(st.on_device_route_params_info(
            dev_id,
            Some(ParamInfoFlags::READWRITE | ParamInfoFlags::SERIAL),
        ));
        assert!(!st.device_routes.contains_key(&(dev_id, out_dir, 5)));

        assert!(st.record_device_route(
            dev_id,
            0x4000_0010,
            out_dir,
            DeviceRouteRecord {
                index: 3,
                device: 9,
                name: Some("analog-output-lineout".into()),
                channels: Some(2),
                volume: Some(0.04),
                channel_volumes: Some(vec![0.04, 0.04]),
                channel_map: None,
                muted: Some(false),
            },
        ));
        assert!(st.record_device_route(
            dev_id,
            0x4000_0011,
            out_dir,
            DeviceRouteRecord {
                index: 4,
                device: 5,
                name: Some("analog-output-headphones".into()),
                channels: Some(2),
                volume: Some(0.125),
                channel_volumes: Some(vec![0.125, 0.125]),
                channel_map: None,
                muted: Some(false),
            },
        ));
        assert!(!st.device_routes.contains_key(&(dev_id, out_dir, 9)));
        assert_eq!(st.device_routes[&(dev_id, out_dir, 5)].index, 4);

        assert!(!st.on_device_route_params_info(dev_id, None));
        assert!(st.device_routes.is_empty());
        assert!(!st.record_device_route(
            dev_id,
            0x4000_0012,
            out_dir,
            DeviceRouteRecord {
                index: 4,
                device: 5,
                name: Some("analog-output-headphones".into()),
                channels: Some(2),
                volume: Some(0.125),
                channel_volumes: Some(vec![0.125, 0.125]),
                channel_map: None,
                muted: Some(false),
            },
        ));
        assert!(st.device_routes.is_empty());
    }

    #[test]
    fn route_resolution_isolation_mono_channels_and_props_precedence() {
        use super::super::pod::{ProfileParams, PropsExtracted, RouteParams, linear_to_raw};
        use super::{AudioNodeRecord, DeviceCardRecord, DeviceRouteRecord, WorkerState};
        use crate::audio::types::{AudioDirection, AudioPortAvailability};
        use pipewire::spa::param::ParamInfoFlags;
        use pipewire::spa::sys as spa_sys;

        let mut st = WorkerState::default();
        let out_dir = spa_sys::SPA_DIRECTION_OUTPUT;
        let in_dir = spa_sys::SPA_DIRECTION_INPUT;

        st.device_cards.insert(
            45,
            DeviceCardRecord {
                id: 45,
                name: "alsa_card.pci-0000_0c_00.6".into(),
                description: "ALC1220".into(),
                ..DeviceCardRecord::default()
            },
        );
        let (er_ok, prof_ok, ep_ok) = st.on_device_card_params_info(
            45,
            Some(ParamInfoFlags::READ),
            Some(ParamInfoFlags::READWRITE),
            Some(ParamInfoFlags::READ),
        );
        assert!(er_ok && prof_ok && ep_ok);
        assert!(st.record_device_enum_profile(
            45,
            10,
            ProfileParams {
                index: 1,
                name: "output:analog-stereo+input:analog-stereo".into(),
                description: "Analog Stereo Duplex".into(),
                priority: 6565,
                available: true,
            },
        ));
        assert!(st.record_device_profile(
            45,
            ProfileParams {
                index: 1,
                name: "output:analog-stereo+input:analog-stereo".into(),
                description: "Analog Stereo Duplex".into(),
                priority: 6565,
                available: true,
            },
        ));
        assert!(st.record_device_enum_route(
            45,
            11,
            RouteParams {
                index: 3,
                direction: Some(out_dir),
                device: None,
                name: Some("analog-output-lineout".into()),
                description: Some("Line Out".into()),
                priority: 9000,
                availability: AudioPortAvailability::Available,
                profiles: vec![1, 2],
                devices: vec![5],
                channels: None,
                volume: None,
                channel_volumes: None,
                channel_map: None,
                muted: None,
            },
        ));
        assert!(st.record_device_enum_route(
            45,
            11,
            RouteParams {
                index: 4,
                direction: Some(out_dir),
                device: None,
                name: Some("analog-output-headphones".into()),
                description: Some("Headphones".into()),
                priority: 9900,
                availability: AudioPortAvailability::Available,
                profiles: vec![1, 2],
                devices: vec![5],
                channels: None,
                volume: None,
                channel_volumes: None,
                channel_map: None,
                muted: None,
            },
        ));

        assert!(st.on_device_route_params_info(45, Some(ParamInfoFlags::READWRITE)));
        let raw_80 = linear_to_raw(0.8);
        let raw_40 = linear_to_raw(0.4);
        assert!(st.record_device_route(
            45,
            0x4000_0001,
            out_dir,
            DeviceRouteRecord {
                index: 4,
                device: 5,
                name: Some("analog-output-headphones".into()),
                channels: Some(2),
                volume: Some(raw_80),
                channel_volumes: Some(vec![raw_80, raw_40]),
                channel_map: None,
                muted: Some(false),
            },
        ));

        assert!(st.on_device_route_params_info(44, Some(ParamInfoFlags::READWRITE)));
        assert!(st.record_device_route(
            44,
            0x4000_0002,
            in_dir,
            DeviceRouteRecord {
                index: 0,
                device: 1,
                name: Some("analog-input-mic".into()),
                channels: Some(1),
                volume: Some(0.5),
                channel_volumes: Some(vec![0.5]),
                channel_map: None,
                muted: Some(false),
            },
        ));

        let mut analog_sink = AudioNodeRecord::new(
            57,
            "analog".into(),
            "Analog".into(),
            AudioDirection::Output,
            Some(45),
            Some(5),
        );
        let route_45 = st.find_route_for_node(&analog_sink).unwrap().clone();
        analog_sink.apply_route(&route_45);

        let spdif_sink = AudioNodeRecord::new(
            59,
            "spdif".into(),
            "S/PDIF".into(),
            AudioDirection::Output,
            Some(45),
            Some(6),
        );
        let mono_mic = AudioNodeRecord::new(
            56,
            "webcam-mic".into(),
            "Webcam".into(),
            AudioDirection::Input,
            Some(44),
            Some(1),
        );

        assert_eq!(
            st.find_route_for_node(&analog_sink).map(|r| r.index),
            Some(4)
        );
        assert!(st.find_route_for_node(&spdif_sink).is_none());

        let mic_route = st.find_route_for_node(&mono_mic).expect("mono route");
        assert_eq!(WorkerState::route_channel_count(&mono_mic, mic_route), 1);

        let scaled =
            WorkerState::target_channel_volumes_for_level(&analog_sink, Some(&route_45), 0.6);
        assert!((scaled[0] - 0.6).abs() < 1e-4);
        assert!((scaled[1] - 0.3).abs() < 1e-4);

        st.sinks.insert(57, analog_sink);
        st.sinks.insert(59, spdif_sink);
        assert!(!st.apply_node_props(
            AudioDirection::Output,
            57,
            PropsExtracted {
                channels: Some(2),
                volume: Some(1.0),
                channel_volumes: Some(vec![1.0, 1.0]),
                channel_map: None,
                muted: Some(true),
            },
        ));
        assert!((st.sinks[&57].raw_volume - raw_80).abs() < 1e-6);
        assert!(!st.sinks[&57].muted);

        assert!(st.apply_node_props(
            AudioDirection::Output,
            59,
            PropsExtracted {
                channels: Some(2),
                volume: Some(0.25),
                channel_volumes: Some(vec![0.25, 0.25]),
                channel_map: None,
                muted: Some(true),
            },
        ));
        assert_eq!(st.sinks[&59].raw_volume, 0.25);
        assert!(st.sinks[&59].muted);

        assert_eq!(
            st.resolve_port_selection(AudioDirection::Output, Some(57), "analog-output-lineout"),
            Some((45, 3, 5))
        );
        assert_eq!(
            st.resolve_profile_selection(45, "output:analog-stereo+input:analog-stereo"),
            Some(1)
        );
        let snap = st.compute_snapshot();
        assert_eq!(snap.cards.len(), 1);
        assert_eq!(
            snap.cards[0].active_profile.as_deref(),
            Some("output:analog-stereo+input:analog-stereo")
        );
        let dev57 = snap.sinks.iter().find(|d| d.id == 57).unwrap();
        assert!(dev57.is_default);
        assert!((dev57.volume.linear - 0.8).abs() < 1e-4);
        assert_eq!(
            dev57.active_port.as_deref(),
            Some("analog-output-headphones")
        );
        assert_eq!(dev57.ports.len(), 2);
        assert_eq!(dev57.ports[0].name, "analog-output-headphones");
        assert_eq!(dev57.ports[1].name, "analog-output-lineout");
    }

    #[test]
    fn default_metadata_prefers_active_default_over_disconnected_configured_sink() {
        use super::{AudioNodeRecord, WorkerState};
        use crate::audio::types::AudioDirection;

        let mut st = WorkerState::default();
        st.sinks.insert(
            55,
            AudioNodeRecord::new(
                55,
                "alsa_output.hdmi".into(),
                "HDMI".into(),
                AudioDirection::Output,
                Some(42),
                Some(15),
            ),
        );
        st.sinks.insert(
            57,
            AudioNodeRecord::new(
                57,
                "alsa_output.alc1220".into(),
                "ALC1220".into(),
                AudioDirection::Output,
                Some(45),
                Some(5),
            ),
        );

        assert!(st.update_default_metadata(
            "default.audio.sink",
            Some(r#"{"name":"alsa_output.alc1220"}"#),
        ));
        assert!(st.update_default_metadata(
            "default.configured.audio.sink",
            Some(r#"{"name":"bluez_output.offline"}"#),
        ));
        assert_eq!(
            st.target_node(AudioDirection::Output).map(|n| n.id),
            Some(57)
        );

        assert!(st.update_default_metadata("default.audio.sink", None));
        assert!(st.update_default_metadata(
            "default.configured.audio.sink",
            Some(r#"{"name":"alsa_output.alc1220"}"#),
        ));
        assert_eq!(
            st.target_node(AudioDirection::Output).map(|n| n.id),
            Some(57)
        );
    }
}
