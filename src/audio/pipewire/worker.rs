use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pipewire as pw;
use pw::channel as pw_channel;
use pw::device::Device;
use pw::metadata::Metadata;
use pw::node::Node;
use pw::proxy::{Listener, ProxyListener, ProxyT};
use pw::spa;
use pw::types::ObjectType;
use spa::param::ParamType;
use spa::pod::Pod;
use spa::sys as spa_sys;

use crate::audio::backend::{AudioCommand, SharedAudioState, stream_display_name};
use crate::audio::types::{AudioCapture, AudioDevice, AudioEvent, AudioSnapshot, AudioStream};

use super::pod::{
    build_mute_pod, build_route_mute_pod, build_route_volume_pod, build_volume_pod, escape_json,
    parse_default_node_name, parse_props_pod, parse_route_pod, raw_to_db, raw_to_linear,
};

#[derive(Debug, Clone)]
struct AudioNodeRecord {
    id: u32,
    name: String,
    description: String,
    direction: u32,
    device_id: Option<u32>,
    card_profile_device: Option<i32>,
    channels: usize,
    raw_volume: f32,
    muted: bool,
}

/// Direction of a per-application stream node (`Stream/Output/Audio` plays back,
/// `Stream/Input/Audio` records).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamClass {
    Playback,
    Capture,
}

/// Classifies a PipeWire `media.class` value into a stream direction; only audio streams,
/// matching either exact nodes or suffixed subclasses, are accepted.
fn classify_media_class(class: &str) -> Option<StreamClass> {
    if class.starts_with("Stream/Output/Audio") {
        Some(StreamClass::Playback)
    } else if class.starts_with("Stream/Input/Audio") {
        Some(StreamClass::Capture)
    } else {
        None
    }
}

/// Per-application stream node (`Stream/*/Audio`); volume and mute live on the node Props.
#[derive(Debug, Clone)]
struct AudioStreamRecord {
    id: u32,
    name: String,
    binary: Option<String>,
    target_id: Option<u32>,
    channels: usize,
    raw_volume: f32,
    muted: bool,
    playback: bool,
}

#[derive(Debug, Clone, Copy)]
struct DeviceRouteRecord {
    index: i32,
    device: i32,
    channels: usize,
}

#[derive(Default)]
struct WorkerState {
    sinks: BTreeMap<u32, AudioNodeRecord>,
    sources: BTreeMap<u32, AudioNodeRecord>,
    streams: BTreeMap<u32, AudioStreamRecord>,
    default_sink_name: Option<String>,
    default_source_name: Option<String>,
    last_snapshot: Option<AudioSnapshot>,
    nodes: HashMap<u32, Node>,
    devices: HashMap<u32, Device>,
    device_routes: HashMap<(u32, u32, i32), DeviceRouteRecord>,
    listeners: HashMap<u32, Vec<Box<dyn Listener>>>,
    default_metadata: Option<(u32, Metadata)>,
}

fn summarize_nodes(
    nodes: &BTreeMap<u32, AudioNodeRecord>,
    default_name: Option<&str>,
) -> (
    Vec<AudioDevice>,
    f32,
    Option<f32>,
    bool,
    Option<AudioDevice>,
) {
    let devices: Vec<AudioDevice> = nodes
        .values()
        .map(|rec| AudioDevice {
            id: rec.id,
            name: rec.name.clone(),
            description: rec.description.clone(),
        })
        .collect();

    let target = default_name
        .and_then(|name| nodes.values().find(|s| s.name == name))
        .or_else(|| nodes.values().next());

    let (linear, db, muted, default_dev) = if let Some(node) = target {
        (
            raw_to_linear(node.raw_volume),
            raw_to_db(node.raw_volume),
            node.muted,
            Some(AudioDevice {
                id: node.id,
                name: node.name.clone(),
                description: node.description.clone(),
            }),
        )
    } else {
        (0.0, None, false, None)
    };
    (devices, linear, db, muted, default_dev)
}

impl WorkerState {
    fn stream_lists(&self) -> (Vec<AudioStream>, Vec<AudioCapture>) {
        let mut streams = Vec::new();
        let mut captures = Vec::new();
        for rec in self.streams.values() {
            let linear = raw_to_linear(rec.raw_volume);
            let db = raw_to_db(rec.raw_volume);
            if rec.playback {
                streams.push(AudioStream {
                    id: rec.id,
                    name: rec.name.clone(),
                    binary: rec.binary.clone(),
                    sink_id: rec.target_id,
                    linear,
                    db,
                    muted: rec.muted,
                });
            } else {
                captures.push(AudioCapture {
                    id: rec.id,
                    name: rec.name.clone(),
                    binary: rec.binary.clone(),
                    source_id: rec.target_id,
                    linear,
                    db,
                    muted: rec.muted,
                });
            }
        }
        (streams, captures)
    }

    fn compute_snapshot(&self) -> Option<AudioSnapshot> {
        let (devices, linear, db, muted, default_sink) =
            summarize_nodes(&self.sinks, self.default_sink_name.as_deref());
        let (sources, source_linear, source_db, source_muted, default_source) =
            summarize_nodes(&self.sources, self.default_source_name.as_deref());

        let (streams, captures) = self.stream_lists();

        Some(AudioSnapshot {
            linear,
            db,
            muted,
            default_sink,
            devices,
            source_linear,
            source_db,
            source_muted,
            default_source,
            sources,
            streams,
            captures,
        })
    }

    fn publish_if_changed(&mut self, shared: &Arc<Mutex<SharedAudioState>>) {
        let next = self.compute_snapshot();
        if next.as_ref() == self.last_snapshot.as_ref() {
            return;
        }
        self.last_snapshot = next.clone();
        if let Ok(mut guard) = shared.lock() {
            guard.snapshot = next.clone();
            if let Some(snapshot) = next {
                guard.push_event(AudioEvent::Updated(snapshot));
            }
        }
    }

    fn target_node(&self, is_source: bool) -> Option<&AudioNodeRecord> {
        let (map, default_name) = if is_source {
            (&self.sources, self.default_source_name.as_deref())
        } else {
            (&self.sinks, self.default_sink_name.as_deref())
        };
        default_name
            .and_then(|name| map.values().find(|s| s.name == name))
            .or_else(|| map.values().next())
    }

    fn resolve_route_for_node(
        &self,
        rec: &AudioNodeRecord,
    ) -> Option<(&Device, DeviceRouteRecord)> {
        let dev_obj_id = rec.device_id?;
        let device = self.devices.get(&dev_obj_id)?;
        if let Some(card_dev) = rec.card_profile_device
            && let Some(route) = self
                .device_routes
                .get(&(dev_obj_id, rec.direction, card_dev))
        {
            return Some((device, *route));
        }
        let mut matches = self
            .device_routes
            .iter()
            .filter(|&(&(d, dir, _), _)| d == dev_obj_id && dir == rec.direction)
            .map(|(_, route)| *route);
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some((device, first))
    }

    fn set_node_volume(&self, is_source: bool, level: f32) {
        self.apply_node_param(
            is_source,
            |rec, route| {
                build_route_volume_pod(
                    route.index,
                    route.device,
                    level,
                    route.channels.max(rec.channels).max(1),
                )
            },
            |rec| build_volume_pod(level, rec.channels.max(1)),
        );
    }

    fn set_node_muted(&self, is_source: bool, muted: bool) {
        self.apply_node_param(
            is_source,
            |_rec, route| build_route_mute_pod(route.index, route.device, muted),
            |_rec| build_mute_pod(muted),
        );
    }

    fn apply_node_param(
        &self,
        is_source: bool,
        build_route: impl FnOnce(&AudioNodeRecord, DeviceRouteRecord) -> Option<Vec<u8>>,
        build_node: impl FnOnce(&AudioNodeRecord) -> Option<Vec<u8>>,
    ) {
        let Some(rec) = self.target_node(is_source) else {
            return;
        };
        if let Some((device, route)) = self.resolve_route_for_node(rec)
            && let Some(bytes) = build_route(rec, route)
            && let Some(pod) = Pod::from_bytes(&bytes)
        {
            device.set_param(ParamType::Route, 0, pod);
            return;
        }
        if let Some(node) = self.nodes.get(&rec.id)
            && let Some(bytes) = build_node(rec)
            && let Some(pod) = Pod::from_bytes(&bytes)
        {
            node.set_param(ParamType::Props, 0, pod);
        }
    }

    fn select_default_node(&self, is_source: bool, id: u32) {
        let (map, meta_key) = if is_source {
            (&self.sources, "default.configured.audio.source")
        } else {
            (&self.sinks, "default.configured.audio.sink")
        };
        if let Some(rec) = map.get(&id)
            && let Some((_, meta)) = &self.default_metadata
        {
            let json = format!("{{\"name\":\"{}\"}}", escape_json(&rec.name));
            meta.set_property(0, meta_key, Some("Spa:String:JSON"), Some(&json));
        }
    }

    /// Sends a Props update carrying either a volume or a mute value to a per-app stream node;
    /// unknown stream IDs are no-ops.
    fn set_stream_volume(&self, id: u32, level: f32) {
        let Some(rec) = self.streams.get(&id) else {
            return;
        };
        if let Some(bytes) = build_volume_pod(level, rec.channels.max(1))
            && let Some(pod) = Pod::from_bytes(&bytes)
            && let Some(node) = self.nodes.get(&id)
        {
            node.set_param(ParamType::Props, 0, pod);
        }
    }

    fn set_stream_muted(&self, id: u32, muted: bool) {
        if let Some(bytes) = build_mute_pod(muted)
            && let Some(pod) = Pod::from_bytes(&bytes)
            && let Some(node) = self.nodes.get(&id)
            && self.streams.contains_key(&id)
        {
            node.set_param(ParamType::Props, 0, pod);
        }
    }

    fn insert_stream_record(
        &mut self,
        id: u32,
        name: String,
        binary: Option<String>,
        target_id: Option<u32>,
        playback: bool,
    ) {
        self.streams.insert(
            id,
            AudioStreamRecord {
                id,
                name,
                binary,
                target_id,
                channels: 2,
                raw_volume: 1.0,
                muted: false,
                playback,
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
                // Reconnection cycle after disconnect; future commands need loop wakeup or new
                // connection setup without rebinding stale channels.
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
            guard.push_event(AudioEvent::Disconnected);
        }

        if result.is_ok() {
            backoff = Duration::from_millis(500);
        } else {
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }

        let mut slept = Duration::ZERO;
        while slept < backoff && !stop_flag.load(Ordering::Acquire) {
            let chunk = Duration::from_millis(50).min(backoff - slept);
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
                guard.push_event(AudioEvent::Reconnected);
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
    let main_loop_cmd = main_loop.downgrade();
    let _cmd_rx = rx.attach(main_loop.loop_(), move |cmd| match cmd {
        AudioCommand::SetVolumeLinear(level) => {
            state_cmd.borrow().set_node_volume(false, level);
        }
        AudioCommand::SetMuted(muted) => {
            state_cmd.borrow().set_node_muted(false, muted);
        }
        AudioCommand::ToggleMute => {
            let st = state_cmd.borrow();
            if let Some(sink) = st.target_node(false) {
                st.set_node_muted(false, !sink.muted);
            }
        }
        AudioCommand::SelectDevice(id) => {
            state_cmd.borrow().select_default_node(false, id);
        }
        AudioCommand::SetSourceVolumeLinear(level) => {
            state_cmd.borrow().set_node_volume(true, level);
        }
        AudioCommand::SetSourceMuted(muted) => {
            state_cmd.borrow().set_node_muted(true, muted);
        }
        AudioCommand::ToggleSourceMute => {
            let st = state_cmd.borrow();
            if let Some(src) = st.target_node(true) {
                st.set_node_muted(true, !src.muted);
            }
        }
        AudioCommand::SelectSource(id) => {
            state_cmd.borrow().select_default_node(true, id);
        }
        AudioCommand::SetStreamVolumeLinear(id, level) => {
            state_cmd.borrow().set_stream_volume(id, level);
        }
        AudioCommand::SetStreamMuted(id, muted) => {
            state_cmd.borrow().set_stream_muted(id, muted);
        }
        AudioCommand::ToggleStreamMute(id) => {
            let st = state_cmd.borrow();
            if let Some(rec) = st.streams.get(&id) {
                st.set_stream_muted(id, !rec.muted);
            }
        }
        AudioCommand::SetCaptureVolumeLinear(id, level) => {
            state_cmd.borrow().set_stream_volume(id, level);
        }
        AudioCommand::SetCaptureMuted(id, muted) => {
            state_cmd.borrow().set_stream_muted(id, muted);
        }
        AudioCommand::ToggleCaptureMute(id) => {
            let st = state_cmd.borrow();
            if let Some(rec) = st.streams.get(&id) {
                st.set_stream_muted(id, !rec.muted);
            }
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
                                if k == "default.audio.sink" || k == "default.configured.audio.sink"
                                {
                                    let parsed = value.and_then(parse_default_node_name);
                                    let mut st = state_meta.borrow_mut();
                                    if parsed.is_some() || k == "default.audio.sink" {
                                        st.default_sink_name = parsed;
                                        st.publish_if_changed(&shared_meta);
                                    }
                                } else if k == "default.audio.source"
                                    || k == "default.configured.audio.source"
                                {
                                    let parsed = value.and_then(parse_default_node_name);
                                    let mut st = state_meta.borrow_mut();
                                    if parsed.is_some() || k == "default.audio.source" {
                                        st.default_source_name = parsed;
                                        st.publish_if_changed(&shared_meta);
                                    }
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
                    let state_dev = Rc::clone(&state_global);
                    let shared_dev = Arc::clone(&shared_global);
                    let listener = device
                        .add_listener_local()
                        .param(move |_seq, param_id, _index, _next, param| {
                            if param_id != ParamType::Route {
                                return;
                            }
                            let Some(pod) = param else {
                                return;
                            };
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
                            let mut st = state_dev.borrow_mut();
                            let channels = route.channels.unwrap_or(2).max(1);
                            st.device_routes.insert(
                                (id, direction, route.device),
                                DeviceRouteRecord {
                                    index: route.index,
                                    device: route.device,
                                    channels,
                                },
                            );
                            let mut updated = false;
                            let target_map = if direction == spa_sys::SPA_DIRECTION_INPUT {
                                &mut st.sources
                            } else {
                                &mut st.sinks
                            };
                            for node_rec in target_map.values_mut() {
                                if node_rec.device_id == Some(id)
                                    && node_rec.card_profile_device == Some(route.device)
                                {
                                    if let Some(ch) = route.channels {
                                        node_rec.channels = ch;
                                        updated = true;
                                    }
                                    if let Some(vol) = route.volume {
                                        node_rec.raw_volume = vol;
                                        updated = true;
                                    }
                                    if let Some(m) = route.muted {
                                        node_rec.muted = m;
                                        updated = true;
                                    }
                                }
                            }
                            if updated {
                                st.publish_if_changed(&shared_dev);
                            }
                        })
                        .register();

                    device.subscribe_params(&[ParamType::Route]);
                    device.enum_params(0, Some(ParamType::Route), 0, u32::MAX);

                    let state_removed = Rc::downgrade(&state_global);
                    let proxy_listener: ProxyListener = device
                        .upcast_ref()
                        .add_listener_local()
                        .removed(move || {
                            if let Some(st_rc) = state_removed.upgrade() {
                                let mut st = st_rc.borrow_mut();
                                st.devices.remove(&id);
                                st.device_routes.retain(|&(dev_id, _, _), _| dev_id != id);
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
                    let is_source = props.get("media.class") == Some("Audio/Source");
                    let direction = if is_source {
                        spa_sys::SPA_DIRECTION_INPUT
                    } else {
                        spa_sys::SPA_DIRECTION_OUTPUT
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

                    let device = AudioDevice {
                        id,
                        name: name.clone(),
                        description: description.clone(),
                    };

                    {
                        let mut st = state_global.borrow_mut();
                        let rec = AudioNodeRecord {
                            id,
                            name,
                            description,
                            direction,
                            device_id,
                            card_profile_device,
                            channels: 2,
                            raw_volume: 1.0,
                            muted: false,
                        };
                        if is_source {
                            st.sources.insert(id, rec);
                        } else {
                            st.sinks.insert(id, rec);
                            if let Ok(mut guard) = shared_global.lock() {
                                guard.push_event(AudioEvent::DeviceAdded(device));
                            }
                        }
                        st.publish_if_changed(&shared_global);
                    }

                    let state_info = Rc::clone(&state_global);
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
                            let map = if is_source {
                                &mut st.sources
                            } else {
                                &mut st.sinks
                            };
                            if let Some(rec) = map.get_mut(&id) {
                                if dev_id.is_some() {
                                    rec.device_id = dev_id;
                                }
                                if card_dev.is_some() {
                                    rec.card_profile_device = card_dev;
                                }
                            }
                        })
                        .param(move |_seq, param_id, _index, _next, param| {
                            if param_id != ParamType::Props {
                                return;
                            }
                            let Some(pod) = param else {
                                return;
                            };
                            let props_parsed = parse_props_pod(pod.as_bytes());
                            if let Some((channels, raw_vol, muted)) = props_parsed {
                                let mut st = state_param.borrow_mut();
                                let map = if is_source {
                                    &mut st.sources
                                } else {
                                    &mut st.sinks
                                };
                                if let Some(rec) = map.get_mut(&id) {
                                    if let Some(ch) = channels {
                                        rec.channels = ch;
                                    }
                                    if let Some(vol) = raw_vol {
                                        rec.raw_volume = vol;
                                    }
                                    if let Some(m) = muted {
                                        rec.muted = m;
                                    }
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
                ObjectType::Node
                    if classify_media_class(props.get("media.class").unwrap_or_default())
                        .is_some() =>
                {
                    let playback =
                        classify_media_class(props.get("media.class").unwrap_or_default())
                            == Some(StreamClass::Playback);
                    let Ok(node): std::result::Result<Node, _> = registry.bind(obj) else {
                        return;
                    };
                    let id = obj.id;
                    let name = stream_display_name(
                        props.get("application.name"),
                        props.get("application.process.binary"),
                        Some(props.get("node.name").unwrap_or("stream")),
                    );
                    let binary = props.get("application.process.binary").map(str::to_string);
                    let target_id = props
                        .get("node.target")
                        .or_else(|| props.get("target.node"))
                        .and_then(|s| s.split_whitespace().next())
                        .and_then(|s| s.parse::<u32>().ok());

                    {
                        let mut st = state_global.borrow_mut();
                        st.insert_stream_record(id, name, binary, target_id, playback);
                        st.publish_if_changed(&shared_global);
                    }

                    let state_param = Rc::clone(&state_global);
                    let shared_param = Arc::clone(&shared_global);
                    let listener = node
                        .add_listener_local()
                        .param(move |_seq, param_id, _index, _next, param| {
                            if param_id != ParamType::Props {
                                return;
                            }
                            let Some(pod) = param else {
                                return;
                            };
                            if let Some((channels, raw_vol, muted)) =
                                parse_props_pod(pod.as_bytes())
                            {
                                let mut st = state_param.borrow_mut();
                                if let Some(rec) = st.streams.get_mut(&id) {
                                    if let Some(ch) = channels {
                                        rec.channels = ch;
                                    }
                                    if let Some(vol) = raw_vol {
                                        rec.raw_volume = vol;
                                    }
                                    if let Some(m) = muted {
                                        rec.muted = m;
                                    }
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
            st.devices.remove(&id);
            st.device_routes.retain(|&(dev_id, _, _), _| dev_id != id);
            st.listeners.remove(&id);
            if removed_sink {
                if let Ok(mut guard) = shared_remove.lock() {
                    guard.push_event(AudioEvent::DeviceRemoved(id));
                }
                st.publish_if_changed(&shared_remove);
            } else if removed_source || removed_stream {
                st.publish_if_changed(&shared_remove);
            }
        })
        .register();

    main_loop.run();

    let mut st = state.borrow_mut();
    st.default_metadata = None;
    st.nodes.clear();
    st.devices.clear();
    st.device_routes.clear();
    st.streams.clear();
    st.listeners.clear();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::StreamClass;
    use super::classify_media_class;

    #[test]
    fn classify_media_class_accepts_audio_playback_streams_only() {
        assert_eq!(
            classify_media_class("Stream/Output/Audio"),
            Some(StreamClass::Playback)
        );
        assert_eq!(
            classify_media_class("Stream/Input/Audio"),
            Some(StreamClass::Capture)
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
}
