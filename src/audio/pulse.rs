//! PulseAudio (`libpulse-binding`) audio backend implementation.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use libpulse_binding as pa;
use pa::callbacks::ListResult;
use pa::context::subscribe::InterestMaskSet;
use pa::context::{Context, FlagSet, State as ContextState};
use pa::mainloop::threaded::Mainloop;
use pa::volume::{ChannelVolumes, Volume, VolumeDB, VolumeLinear};

use crate::error::{Result, WaywiError};

use super::backend::{
    AudioBackend, AudioCommand, AudioCommandSender, SharedAudioState, StreamMetadata,
    update_snapshot,
};
use super::types::{
    AudioCard, AudioChannelPosition, AudioDevice, AudioDirection, AudioEvent, AudioPort,
    AudioPortAvailability, AudioPortTarget, AudioProfile, AudioSnapshot, AudioStream, AudioTarget,
    AudioVolume, apply_linear_balance_with_map, sanitize_linear_channels, scale_linear_channels,
};

/// Volume and channel-map state shared between PulseAudio devices and per-application streams.
#[derive(Debug, Clone)]
struct PulseVolumeState {
    channels: u8,
    volume_linear: f32,
    channel_volumes: Vec<f32>,
    channel_map: Vec<AudioChannelPosition>,
    nonzero_template: Vec<f32>,
    db: Option<f32>,
    muted: bool,
}

impl PulseVolumeState {
    fn from_pa(volume: &ChannelVolumes, raw_map: &pa::channelmap::Map, muted: bool) -> Self {
        let (volume_linear, channel_volumes, db) = pulse_volume_parts(volume);
        let channel_map = pulse_channel_map(raw_map, channel_volumes.len());
        let nonzero_template = if channel_volumes.iter().any(|&v| v > 1e-6) {
            channel_volumes.clone()
        } else {
            vec![1.0; channel_volumes.len().max(1)]
        };
        Self {
            channels: volume.len().max(1),
            volume_linear,
            channel_volumes,
            channel_map,
            nonzero_template,
            db,
            muted,
        }
    }

    fn to_audio_volume(&self) -> AudioVolume {
        AudioVolume {
            linear: self.volume_linear,
            channels: self.channel_volumes.clone(),
            channel_map: self.channel_map.clone(),
            db: self.db,
            muted: self.muted,
        }
    }

    fn update_channels_optimistic(&mut self, channels: &[f32]) {
        if channels.is_empty() {
            return;
        }
        let peak = channels.iter().copied().fold(0.0_f32, f32::max);
        self.volume_linear = peak;
        self.channels = channels.len().min(u8::MAX as usize) as u8;
        self.channel_volumes.clear();
        self.channel_volumes.extend_from_slice(channels);
        if peak > 1e-6 {
            self.nonzero_template.clear();
            self.nonzero_template.extend_from_slice(channels);
        }
    }

    fn scale_for_level(&self, linear: f32) -> Vec<f32> {
        scale_linear_channels(
            &self.channel_volumes,
            &self.nonzero_template,
            self.channels as usize,
            linear,
        )
    }

    fn scale_for_balance(&self, balance: f32) -> Vec<f32> {
        apply_linear_balance_with_map(
            &self.channel_volumes,
            &self.channel_map,
            self.channels as usize,
            self.volume_linear,
            balance,
        )
    }
}

/// Device record shared by output sinks and input sources (mics).
#[derive(Debug, Clone)]
struct PulseDeviceRecord {
    id: u32,
    name: String,
    description: String,
    vol: PulseVolumeState,
    active_port: Option<String>,
    ports: Vec<AudioPort>,
}

/// Per-application stream record shared by sink inputs (playback) and source outputs (capture).
#[derive(Debug, Clone)]
struct PulseStreamRecord {
    id: u32,
    direction: AudioDirection,
    name: String,
    app_name: Option<String>,
    binary_name: Option<String>,
    media_role: Option<String>,
    target_id: u32,
    corked: bool,
    vol: PulseVolumeState,
}

/// Device and per-app stream state owned by the PulseAudio worker; rebuilt from introspection
/// batches and re-published as a snapshot whenever any part changes.
#[derive(Default)]
struct PulseRecordState {
    default_sink_name: Option<String>,
    default_source_name: Option<String>,
    sinks: BTreeMap<u32, PulseDeviceRecord>,
    sources: BTreeMap<u32, PulseDeviceRecord>,
    cards: BTreeMap<u32, AudioCard>,
    sink_inputs: BTreeMap<u32, PulseStreamRecord>,
    source_outputs: BTreeMap<u32, PulseStreamRecord>,
}

impl PulseRecordState {
    fn device_map(&self, dir: AudioDirection) -> &BTreeMap<u32, PulseDeviceRecord> {
        match dir {
            AudioDirection::Output => &self.sinks,
            AudioDirection::Input => &self.sources,
        }
    }

    fn device_map_mut(&mut self, dir: AudioDirection) -> &mut BTreeMap<u32, PulseDeviceRecord> {
        match dir {
            AudioDirection::Output => &mut self.sinks,
            AudioDirection::Input => &mut self.sources,
        }
    }

    fn stream_map_mut(&mut self, dir: AudioDirection) -> &mut BTreeMap<u32, PulseStreamRecord> {
        match dir {
            AudioDirection::Output => &mut self.sink_inputs,
            AudioDirection::Input => &mut self.source_outputs,
        }
    }

    fn default_device_id(&self, dir: AudioDirection) -> Option<u32> {
        let name = match dir {
            AudioDirection::Output => self.default_sink_name.as_deref(),
            AudioDirection::Input => self.default_source_name.as_deref(),
        };
        resolve_target(name, self.device_map(dir)).map(|d| d.id)
    }

    fn publish(
        &self,
        shared: &Arc<Mutex<SharedAudioState>>,
        last_snapshot: &mut Option<Arc<AudioSnapshot>>,
    ) {
        let default_sink_id = self.default_device_id(AudioDirection::Output);
        let default_source_id = self.default_device_id(AudioDirection::Input);

        let sinks: Vec<AudioDevice> = self
            .sinks
            .values()
            .map(|rec| rec_device(rec, Some(rec.id) == default_sink_id))
            .collect();
        let sources: Vec<AudioDevice> = self
            .sources
            .values()
            .map(|rec| rec_device(rec, Some(rec.id) == default_source_id))
            .collect();
        let cards: Vec<AudioCard> = self.cards.values().cloned().collect();
        let streams: Vec<AudioStream> = self
            .sink_inputs
            .values()
            .chain(self.source_outputs.values())
            .map(rec_stream)
            .collect();

        let snapshot = AudioSnapshot {
            sinks,
            sources,
            default_sink_id,
            default_source_id,
            streams,
            cards,
        };

        update_snapshot(shared, last_snapshot, snapshot);
    }
}

enum PulseWorkerMsg {
    StateChanged,
    Refresh,
    ServerInfo {
        sink: Option<String>,
        source: Option<String>,
    },
    DeviceBatch {
        direction: AudioDirection,
        devices: Vec<PulseDeviceRecord>,
    },
    CardBatch {
        cards: Vec<AudioCard>,
    },
    StreamBatch {
        direction: AudioDirection,
        streams: Vec<PulseStreamRecord>,
    },
    Cmd(AudioCommand),
}

struct PulseCommandSender {
    tx: mpsc::Sender<PulseWorkerMsg>,
}

impl AudioCommandSender for PulseCommandSender {
    fn send(&self, cmd: AudioCommand) -> Result<()> {
        self.tx
            .send(PulseWorkerMsg::Cmd(cmd))
            .map_err(|_| WaywiError::AudioError("pulseaudio worker channel disconnected".into()))
    }
}

/// PulseAudio [`AudioBackend`] driven by `libpulse_binding::mainloop::threaded::Mainloop`.
pub(crate) struct PulseBackend;

impl AudioBackend for PulseBackend {
    fn start(
        &self,
        shared: Arc<Mutex<SharedAudioState>>,
        stop_flag: Arc<AtomicBool>,
    ) -> Option<(Box<dyn AudioCommandSender>, JoinHandle<()>)> {
        let (tx, rx) = mpsc::channel::<PulseWorkerMsg>();
        let worker_tx = tx.clone();

        let join = thread::Builder::new()
            .name("waywi-pulseaudio".into())
            .spawn(move || run_pulse_worker(shared, stop_flag, worker_tx, rx))
            .ok()?;

        Some((Box::new(PulseCommandSender { tx }), join))
    }
}

fn run_pulse_worker(
    shared: Arc<Mutex<SharedAudioState>>,
    stop_flag: Arc<AtomicBool>,
    tx: mpsc::Sender<PulseWorkerMsg>,
    rx: mpsc::Receiver<PulseWorkerMsg>,
) {
    let Some(mut ml) = Mainloop::new() else {
        return;
    };
    let Some(mut ctx) = Context::new(&ml, "waywi") else {
        return;
    };

    let tx_state = tx.clone();
    ctx.set_state_callback(Some(Box::new(move || {
        let _ = tx_state.send(PulseWorkerMsg::StateChanged);
    })));

    if ctx.connect(None, FlagSet::NOFLAGS, None).is_err() {
        return;
    }
    if ml.start().is_err() {
        return;
    }

    let mut ready = false;
    let mut subscribed = false;
    let mut last_snapshot: Option<Arc<AudioSnapshot>> = None;
    let mut records = PulseRecordState::default();

    while !stop_flag.load(Ordering::Acquire) {
        let Ok(msg) = rx.recv() else {
            break;
        };

        match msg {
            PulseWorkerMsg::Cmd(AudioCommand::Shutdown) => break,
            PulseWorkerMsg::StateChanged => {
                ml.lock();
                let st = ctx.get_state();
                ml.unlock();

                match st {
                    ContextState::Ready if !ready => {
                        ready = true;
                        if let Ok(mut guard) = shared.lock() {
                            guard.connected = true;
                            guard.push_event(AudioEvent::BackendReconnected);
                        }
                        if !subscribed {
                            subscribed = true;
                            ml.lock();
                            let tx_sub = tx.clone();
                            ctx.set_subscribe_callback(Some(Box::new(move |_, _, _| {
                                let _ = tx_sub.send(PulseWorkerMsg::Refresh);
                            })));
                            let _op = ctx.subscribe(
                                InterestMaskSet::SINK
                                    | InterestMaskSet::SERVER
                                    | InterestMaskSet::SOURCE
                                    | InterestMaskSet::CARD
                                    | InterestMaskSet::SINK_INPUT
                                    | InterestMaskSet::SOURCE_OUTPUT,
                                |_| {},
                            );
                            ml.unlock();
                        }
                        request_refresh(&mut ml, &ctx, &tx);
                    }
                    ContextState::Failed | ContextState::Terminated => {
                        if ready && let Ok(mut guard) = shared.lock() {
                            guard.connected = false;
                            guard.push_event(AudioEvent::BackendDisconnected);
                        }
                        break;
                    }
                    _ => {}
                }
            }
            PulseWorkerMsg::Refresh => {
                if ready {
                    request_refresh(&mut ml, &ctx, &tx);
                }
            }
            PulseWorkerMsg::ServerInfo { sink, source } => {
                records.default_sink_name = sink;
                records.default_source_name = source;
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::DeviceBatch { direction, devices } => {
                let map = records.device_map_mut(direction);
                let mut prev = std::mem::take(map);
                *map = devices
                    .into_iter()
                    .map(|mut rec| {
                        let prev_tpl = prev.remove(&rec.id).map(|p| p.vol.nonzero_template);
                        rec.vol.nonzero_template =
                            merge_nonzero_template(prev_tpl, &rec.vol.channel_volumes);
                        (rec.id, rec)
                    })
                    .collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::CardBatch { cards } => {
                records.cards = cards.into_iter().map(|c| (c.id, c)).collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::StreamBatch { direction, streams } => {
                let map = records.stream_map_mut(direction);
                let mut prev = std::mem::take(map);
                *map = streams
                    .into_iter()
                    .map(|mut rec| {
                        let prev_tpl = prev.remove(&rec.id).map(|p| p.vol.nonzero_template);
                        rec.vol.nonzero_template =
                            merge_nonzero_template(prev_tpl, &rec.vol.channel_volumes);
                        (rec.id, rec)
                    })
                    .collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::Cmd(cmd) => {
                if !ready {
                    continue;
                }
                apply_pulse_command(&mut ml, &mut ctx, cmd, &mut records);
            }
        }
    }

    ml.lock();
    ctx.set_state_callback(None);
    ctx.set_subscribe_callback(None);
    ctx.disconnect();
    ml.unlock();
    ml.stop();
}

fn collect_ports<'a>(
    iter: impl Iterator<
        Item = (
            Option<&'a str>,
            Option<&'a str>,
            u32,
            pa::def::PortAvailable,
        ),
    >,
) -> Vec<AudioPort> {
    iter.filter_map(|(name, desc, priority, avail)| {
        let port_name = name.filter(|s| !s.is_empty())?;
        let port_desc = desc.filter(|s| !s.is_empty()).unwrap_or(port_name);
        Some(AudioPort {
            index: 0,
            name: port_name.to_owned(),
            description: port_desc.to_owned(),
            priority,
            availability: pulse_port_availability(avail),
        })
    })
    .collect()
}

fn request_refresh(ml: &mut Mainloop, ctx: &Context, tx: &mpsc::Sender<PulseWorkerMsg>) {
    ml.lock();
    let introspect = ctx.introspect();

    let tx_srv = tx.clone();
    let _srv_op = introspect.get_server_info(move |info| {
        let _ = tx_srv.send(PulseWorkerMsg::ServerInfo {
            sink: info.default_sink_name.as_deref().map(str::to_owned),
            source: info.default_source_name.as_deref().map(str::to_owned),
        });
    });

    let collected_sinks = Arc::new(Mutex::new(Vec::<PulseDeviceRecord>::new()));
    let collected_sinks_cb = Arc::clone(&collected_sinks);
    let tx_sinks = tx.clone();
    let _sink_op = introspect.get_sink_info_list(move |res| match res {
        ListResult::Item(sink) => {
            let ports = collect_ports(sink.ports.iter().map(|p| {
                (
                    p.name.as_deref(),
                    p.description.as_deref(),
                    p.priority,
                    p.available,
                )
            }));
            if let Ok(mut list) = collected_sinks_cb.lock()
                && let Some(rec) = pulse_device_record(
                    sink.index,
                    sink.name.as_deref(),
                    sink.description.as_deref(),
                    &sink.volume,
                    &sink.channel_map,
                    sink.mute,
                    sink.active_port
                        .as_ref()
                        .and_then(|p| p.name.as_deref())
                        .filter(|s| !s.is_empty()),
                    ports,
                )
            {
                list.push(rec);
            }
        }
        ListResult::End => {
            if let Ok(mut list) = collected_sinks_cb.lock() {
                let devices = std::mem::take(&mut *list);
                let _ = tx_sinks.send(PulseWorkerMsg::DeviceBatch {
                    direction: AudioDirection::Output,
                    devices,
                });
            }
        }
        ListResult::Error => {}
    });

    let collected_sources = Arc::new(Mutex::new(Vec::<PulseDeviceRecord>::new()));
    let collected_sources_cb = Arc::clone(&collected_sources);
    let tx_sources = tx.clone();
    let _source_op = introspect.get_source_info_list(move |res| match res {
        ListResult::Item(source) => {
            // Skip sink-monitor sources; they are loopback copies of outputs, not mics.
            if source.monitor_of_sink.is_some() {
                return;
            }
            let ports = collect_ports(source.ports.iter().map(|p| {
                (
                    p.name.as_deref(),
                    p.description.as_deref(),
                    p.priority,
                    p.available,
                )
            }));
            if let Ok(mut list) = collected_sources_cb.lock()
                && let Some(rec) = pulse_device_record(
                    source.index,
                    source.name.as_deref(),
                    source.description.as_deref(),
                    &source.volume,
                    &source.channel_map,
                    source.mute,
                    source
                        .active_port
                        .as_ref()
                        .and_then(|p| p.name.as_deref())
                        .filter(|s| !s.is_empty()),
                    ports,
                )
            {
                list.push(rec);
            }
        }
        ListResult::End => {
            if let Ok(mut list) = collected_sources_cb.lock() {
                let devices = std::mem::take(&mut *list);
                let _ = tx_sources.send(PulseWorkerMsg::DeviceBatch {
                    direction: AudioDirection::Input,
                    devices,
                });
            }
        }
        ListResult::Error => {}
    });

    let collected_cards = Arc::new(Mutex::new(Vec::<AudioCard>::new()));
    let collected_cards_cb = Arc::clone(&collected_cards);
    let tx_cards = tx.clone();
    let _card_op = introspect.get_card_info_list(move |res| match res {
        ListResult::Item(card) => {
            if let Ok(mut list) = collected_cards_cb.lock()
                && let Some(rec) = pulse_card_record(card)
            {
                list.push(rec);
            }
        }
        ListResult::End => {
            if let Ok(mut list) = collected_cards_cb.lock() {
                let cards = std::mem::take(&mut *list);
                let _ = tx_cards.send(PulseWorkerMsg::CardBatch { cards });
            }
        }
        ListResult::Error => {}
    });

    let collected_inputs = Arc::new(Mutex::new(Vec::<PulseStreamRecord>::new()));
    let collected_inputs_cb = Arc::clone(&collected_inputs);
    let tx_inputs = tx.clone();
    let _input_op = introspect.get_sink_input_info_list(move |res| match res {
        ListResult::Item(info) => {
            if info.name.as_deref() == Some("waywi-audio") {
                return;
            }
            if let Ok(mut list) = collected_inputs_cb.lock() {
                list.push(pulse_stream_record(
                    info.index,
                    AudioDirection::Output,
                    &info.proplist,
                    info.name.as_deref(),
                    info.sink,
                    info.corked,
                    &info.volume,
                    &info.channel_map,
                    info.mute,
                ));
            }
        }
        ListResult::End => {
            if let Ok(mut list) = collected_inputs_cb.lock() {
                let streams = std::mem::take(&mut *list);
                let _ = tx_inputs.send(PulseWorkerMsg::StreamBatch {
                    direction: AudioDirection::Output,
                    streams,
                });
            }
        }
        ListResult::Error => {}
    });

    let collected_outputs = Arc::new(Mutex::new(Vec::<PulseStreamRecord>::new()));
    let collected_outputs_cb = Arc::clone(&collected_outputs);
    let tx_outputs = tx.clone();
    let _output_op = introspect.get_source_output_info_list(move |res| match res {
        ListResult::Item(info) => {
            if info.name.as_deref() == Some("waywi-capture") {
                return;
            }
            if let Ok(mut list) = collected_outputs_cb.lock() {
                list.push(pulse_stream_record(
                    info.index,
                    AudioDirection::Input,
                    &info.proplist,
                    info.name.as_deref(),
                    info.source,
                    info.corked,
                    &info.volume,
                    &info.channel_map,
                    info.mute,
                ));
            }
        }
        ListResult::End => {
            if let Ok(mut list) = collected_outputs_cb.lock() {
                let streams = std::mem::take(&mut *list);
                let _ = tx_outputs.send(PulseWorkerMsg::StreamBatch {
                    direction: AudioDirection::Input,
                    streams,
                });
            }
        }
        ListResult::Error => {}
    });

    ml.unlock();
}

enum VolumeMutation {
    SetLinear(f32),
    AdjustLinear(f32),
    SetBalance(f32),
    SetChannels(Vec<f32>),
}

impl VolumeMutation {
    fn apply(&self, vol: &mut PulseVolumeState) -> Option<ChannelVolumes> {
        let next = match self {
            Self::SetLinear(level) => vol.scale_for_level(*level),
            Self::AdjustLinear(delta) => {
                let target = (vol.volume_linear + delta).clamp(0.0, 1.5);
                vol.scale_for_level(target)
            }
            Self::SetBalance(balance) => vol.scale_for_balance(*balance),
            Self::SetChannels(channels) => sanitize_linear_channels(channels)?,
        };
        vol.update_channels_optimistic(&next);
        Some(pulse_channel_volumes_from_slice(&next))
    }
}

fn apply_target_volume(
    ctx: &mut Context,
    records: &mut PulseRecordState,
    target: AudioTarget,
    mutation: VolumeMutation,
) {
    let mut introspect = ctx.introspect();
    match target {
        AudioTarget::DefaultSink | AudioTarget::Sink(_) => {
            let id = match target {
                AudioTarget::Sink(id) => Some(id),
                _ => records.default_device_id(AudioDirection::Output),
            };
            if let Some(id) = id
                && let Some(sink) = records.sinks.get_mut(&id)
                && let Some(cv) = mutation.apply(&mut sink.vol)
            {
                let _op = introspect.set_sink_volume_by_name(&sink.name, &cv, None);
            }
        }
        AudioTarget::DefaultSource | AudioTarget::Source(_) => {
            let id = match target {
                AudioTarget::Source(id) => Some(id),
                _ => records.default_device_id(AudioDirection::Input),
            };
            if let Some(id) = id
                && let Some(source) = records.sources.get_mut(&id)
                && let Some(cv) = mutation.apply(&mut source.vol)
            {
                let _op = introspect.set_source_volume_by_name(&source.name, &cv, None);
            }
        }
        AudioTarget::PlaybackStream(id) => {
            if let Some(rec) = records.sink_inputs.get_mut(&id)
                && let Some(cv) = mutation.apply(&mut rec.vol)
            {
                let _op = introspect.set_sink_input_volume(id, &cv, None);
            }
        }
        AudioTarget::CaptureStream(id) => {
            if let Some(rec) = records.source_outputs.get_mut(&id)
                && let Some(cv) = mutation.apply(&mut rec.vol)
            {
                let _op = introspect.set_source_output_volume(id, &cv, None);
            }
        }
    }
}

fn apply_target_mute(
    ctx: &mut Context,
    records: &mut PulseRecordState,
    target: AudioTarget,
    explicit: Option<bool>,
) {
    let mut introspect = ctx.introspect();
    match target {
        AudioTarget::DefaultSink | AudioTarget::Sink(_) => {
            let id = match target {
                AudioTarget::Sink(id) => Some(id),
                _ => records.default_device_id(AudioDirection::Output),
            };
            if let Some(id) = id
                && let Some(sink) = records.sinks.get_mut(&id)
            {
                let next = explicit.unwrap_or(!sink.vol.muted);
                sink.vol.muted = next;
                let _op = introspect.set_sink_mute_by_name(&sink.name, next, None);
            }
        }
        AudioTarget::DefaultSource | AudioTarget::Source(_) => {
            let id = match target {
                AudioTarget::Source(id) => Some(id),
                _ => records.default_device_id(AudioDirection::Input),
            };
            if let Some(id) = id
                && let Some(source) = records.sources.get_mut(&id)
            {
                let next = explicit.unwrap_or(!source.vol.muted);
                source.vol.muted = next;
                let _op = introspect.set_source_mute_by_name(&source.name, next, None);
            }
        }
        AudioTarget::PlaybackStream(id) => {
            if let Some(rec) = records.sink_inputs.get_mut(&id) {
                let next = explicit.unwrap_or(!rec.vol.muted);
                rec.vol.muted = next;
                let _op = introspect.set_sink_input_mute(id, next, None);
            }
        }
        AudioTarget::CaptureStream(id) => {
            if let Some(rec) = records.source_outputs.get_mut(&id) {
                let next = explicit.unwrap_or(!rec.vol.muted);
                rec.vol.muted = next;
                let _op = introspect.set_source_output_mute(id, next, None);
            }
        }
    }
}

fn apply_pulse_command(
    ml: &mut Mainloop,
    ctx: &mut Context,
    cmd: AudioCommand,
    records: &mut PulseRecordState,
) {
    ml.lock();
    match cmd {
        AudioCommand::SetVolume(target, linear) => {
            apply_target_volume(ctx, records, target, VolumeMutation::SetLinear(linear));
        }
        AudioCommand::AdjustVolume(target, delta) => {
            apply_target_volume(ctx, records, target, VolumeMutation::AdjustLinear(delta));
        }
        AudioCommand::SetBalance(target, balance) => {
            apply_target_volume(ctx, records, target, VolumeMutation::SetBalance(balance));
        }
        AudioCommand::SetChannelVolumes(target, channels) => {
            apply_target_volume(ctx, records, target, VolumeMutation::SetChannels(channels));
        }
        AudioCommand::SetMute(target, muted) => {
            apply_target_mute(ctx, records, target, Some(muted));
        }
        AudioCommand::ToggleMute(target) => {
            apply_target_mute(ctx, records, target, None);
        }
        AudioCommand::SetDefaultDevice(direction, id) => {
            if let Some(dev) = records.device_map(direction).get(&id)
                && is_valid_pa_str(&dev.name)
            {
                match direction {
                    AudioDirection::Output => {
                        let _op = ctx.set_default_sink(&dev.name, |_| {});
                    }
                    AudioDirection::Input => {
                        let _op = ctx.set_default_source(&dev.name, |_| {});
                    }
                }
            }
        }
        AudioCommand::SetPort(target, port_name) => {
            if is_valid_pa_str(&port_name) {
                let (direction, explicit_id) = match target {
                    AudioPortTarget::DefaultSink => (AudioDirection::Output, None),
                    AudioPortTarget::Sink(id) => (AudioDirection::Output, Some(id)),
                    AudioPortTarget::DefaultSource => (AudioDirection::Input, None),
                    AudioPortTarget::Source(id) => (AudioDirection::Input, Some(id)),
                };
                let id = explicit_id.or_else(|| records.default_device_id(direction));
                if let Some(id) = id
                    && let Some(dev) = records.device_map(direction).get(&id)
                    && is_valid_pa_str(&dev.name)
                {
                    let mut introspect = ctx.introspect();
                    match direction {
                        AudioDirection::Output => {
                            let _op = introspect.set_sink_port_by_name(&dev.name, &port_name, None);
                        }
                        AudioDirection::Input => {
                            let _op =
                                introspect.set_source_port_by_name(&dev.name, &port_name, None);
                        }
                    }
                }
            }
        }
        AudioCommand::SetCardProfile(card_id, profile_name) => {
            if is_valid_pa_str(&profile_name) && records.cards.contains_key(&card_id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_card_profile_by_index(card_id, &profile_name, None);
            }
        }
        AudioCommand::MoveStream(dir, stream_id, device_id) => {
            let valid = match dir {
                AudioDirection::Output => {
                    records.sink_inputs.contains_key(&stream_id)
                        && records.sinks.contains_key(&device_id)
                }
                AudioDirection::Input => {
                    records.source_outputs.contains_key(&stream_id)
                        && records.sources.contains_key(&device_id)
                }
            };
            if valid {
                let mut introspect = ctx.introspect();
                match dir {
                    AudioDirection::Output => {
                        let _op = introspect.move_sink_input_by_index(stream_id, device_id, None);
                    }
                    AudioDirection::Input => {
                        let _op =
                            introspect.move_source_output_by_index(stream_id, device_id, None);
                    }
                }
                if let Some(stream) = records.stream_map_mut(dir).get_mut(&stream_id) {
                    stream.target_id = device_id;
                }
            }
        }
        AudioCommand::Shutdown => {}
    }
    ml.unlock();
}

fn is_valid_pa_str(s: &str) -> bool {
    !s.is_empty() && !s.contains('\0')
}

fn merge_nonzero_template(prev: Option<Vec<f32>>, current: &[f32]) -> Vec<f32> {
    if current.iter().any(|&v| v > 1e-6) {
        current.to_vec()
    } else if let Some(prev) =
        prev.filter(|p| p.len() == current.len() && p.iter().any(|&v| v > 1e-6))
    {
        prev
    } else {
        vec![1.0; current.len().max(1)]
    }
}

fn pulse_channel_volumes_from_slice(channels: &[f32]) -> ChannelVolumes {
    let mut cv = ChannelVolumes::default();
    let max_ch = pa::sample::Spec::CHANNELS_MAX as usize;
    let count = channels.len().clamp(1, max_ch);
    cv.set_len(count as u8);
    let dst = cv.get_mut();
    for (idx, slot) in dst.iter_mut().enumerate() {
        let linear = channels.get(idx).copied().unwrap_or(1.0).clamp(0.0, 1.5);
        *slot = Volume::from(VolumeLinear(f64::from(linear)));
    }
    cv
}

/// Resolves the active device of a record map: the default by server name, else the first.
fn resolve_target<'a>(
    default_name: Option<&str>,
    records: &'a BTreeMap<u32, PulseDeviceRecord>,
) -> Option<&'a PulseDeviceRecord> {
    default_name
        .and_then(|name| records.values().find(|s| s.name == name))
        .or_else(|| records.values().next())
}

fn rec_device(rec: &PulseDeviceRecord, is_default: bool) -> AudioDevice {
    AudioDevice {
        id: rec.id,
        name: rec.name.clone(),
        description: rec.description.clone(),
        is_default,
        volume: rec.vol.to_audio_volume(),
        active_port: rec.active_port.clone(),
        ports: rec.ports.clone(),
    }
}

fn rec_stream(rec: &PulseStreamRecord) -> AudioStream {
    AudioStream {
        id: rec.id,
        direction: rec.direction,
        name: rec.name.clone(),
        app_name: rec.app_name.clone(),
        binary_name: rec.binary_name.clone(),
        media_role: rec.media_role.clone(),
        device_id: Some(rec.target_id),
        corked: rec.corked,
        volume: rec.vol.to_audio_volume(),
    }
}

fn pulse_channel_map(
    map: &pa::channelmap::Map,
    fallback_count: usize,
) -> Vec<AudioChannelPosition> {
    use pa::channelmap::Position as P;
    let slice = map.get();
    if slice.is_empty() {
        return AudioChannelPosition::default_map_for_count(fallback_count.max(1));
    }
    slice
        .iter()
        .map(|&pos| match pos {
            P::Mono => AudioChannelPosition::Mono,
            P::FrontLeft => AudioChannelPosition::FrontLeft,
            P::FrontRight => AudioChannelPosition::FrontRight,
            P::FrontCenter => AudioChannelPosition::FrontCenter,
            P::RearCenter => AudioChannelPosition::RearCenter,
            P::RearLeft => AudioChannelPosition::RearLeft,
            P::RearRight => AudioChannelPosition::RearRight,
            P::Lfe => AudioChannelPosition::Lfe,
            P::FrontLeftOfCenter => AudioChannelPosition::FrontLeftCenter,
            P::FrontRightOfCenter => AudioChannelPosition::FrontRightCenter,
            P::SideLeft => AudioChannelPosition::SideLeft,
            P::SideRight => AudioChannelPosition::SideRight,
            P::TopCenter => AudioChannelPosition::TopCenter,
            P::TopFrontLeft => AudioChannelPosition::TopFrontLeft,
            P::TopFrontRight => AudioChannelPosition::TopFrontRight,
            P::TopFrontCenter => AudioChannelPosition::TopFrontCenter,
            P::TopRearLeft => AudioChannelPosition::TopRearLeft,
            P::TopRearRight => AudioChannelPosition::TopRearRight,
            P::TopRearCenter => AudioChannelPosition::TopRearCenter,
            P::Aux0 => AudioChannelPosition::Aux(0),
            P::Aux1 => AudioChannelPosition::Aux(1),
            P::Aux2 => AudioChannelPosition::Aux(2),
            P::Aux3 => AudioChannelPosition::Aux(3),
            P::Aux4 => AudioChannelPosition::Aux(4),
            P::Aux5 => AudioChannelPosition::Aux(5),
            P::Aux6 => AudioChannelPosition::Aux(6),
            P::Aux7 => AudioChannelPosition::Aux(7),
            P::Aux8 => AudioChannelPosition::Aux(8),
            P::Aux9 => AudioChannelPosition::Aux(9),
            P::Aux10 => AudioChannelPosition::Aux(10),
            P::Aux11 => AudioChannelPosition::Aux(11),
            P::Aux12 => AudioChannelPosition::Aux(12),
            P::Aux13 => AudioChannelPosition::Aux(13),
            P::Aux14 => AudioChannelPosition::Aux(14),
            P::Aux15 => AudioChannelPosition::Aux(15),
            P::Aux16 => AudioChannelPosition::Aux(16),
            P::Aux17 => AudioChannelPosition::Aux(17),
            P::Aux18 => AudioChannelPosition::Aux(18),
            P::Aux19 => AudioChannelPosition::Aux(19),
            P::Aux20 => AudioChannelPosition::Aux(20),
            P::Aux21 => AudioChannelPosition::Aux(21),
            P::Aux22 => AudioChannelPosition::Aux(22),
            P::Aux23 => AudioChannelPosition::Aux(23),
            P::Aux24 => AudioChannelPosition::Aux(24),
            P::Aux25 => AudioChannelPosition::Aux(25),
            P::Aux26 => AudioChannelPosition::Aux(26),
            P::Aux27 => AudioChannelPosition::Aux(27),
            P::Aux28 => AudioChannelPosition::Aux(28),
            P::Aux29 => AudioChannelPosition::Aux(29),
            P::Aux30 => AudioChannelPosition::Aux(30),
            P::Aux31 => AudioChannelPosition::Aux(31),
            P::Invalid => AudioChannelPosition::Unknown,
        })
        .collect()
}

fn pulse_volume_parts(volume: &ChannelVolumes) -> (f32, Vec<f32>, Option<f32>) {
    let channels: Vec<f32> = volume
        .get()
        .iter()
        .map(|&v| (VolumeLinear::from(v).0.max(0.0) as f32).clamp(0.0, 1.5))
        .collect();
    let max_vol = volume.max();
    let peak = VolumeLinear::from(max_vol).0.max(0.0) as f32;
    let raw_db = VolumeDB::from(max_vol).0 as f32;
    let channel_volumes = if channels.is_empty() {
        vec![peak]
    } else {
        channels
    };
    (peak, channel_volumes, raw_db.is_finite().then_some(raw_db))
}

fn pulse_port_availability(avail: pa::def::PortAvailable) -> AudioPortAvailability {
    match avail {
        pa::def::PortAvailable::Yes => AudioPortAvailability::Available,
        pa::def::PortAvailable::No => AudioPortAvailability::Unavailable,
        pa::def::PortAvailable::Unknown => AudioPortAvailability::Unknown,
    }
}

/// Builds a device record; `None` when the server reports no usable name.
#[allow(clippy::too_many_arguments)]
fn pulse_device_record(
    id: u32,
    name: Option<&str>,
    description: Option<&str>,
    volume: &ChannelVolumes,
    raw_channel_map: &pa::channelmap::Map,
    muted: bool,
    active_port: Option<&str>,
    mut ports: Vec<AudioPort>,
) -> Option<PulseDeviceRecord> {
    let name = name.filter(|s| !s.is_empty())?;
    let description = description
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_owned();
    ports.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.name.cmp(&b.name))
    });
    Some(PulseDeviceRecord {
        id,
        name: name.to_owned(),
        description,
        vol: PulseVolumeState::from_pa(volume, raw_channel_map, muted),
        active_port: active_port.map(str::to_owned),
        ports,
    })
}

fn pulse_card_record(card: &pa::context::introspect::CardInfo<'_>) -> Option<AudioCard> {
    let name = card.name.as_deref().filter(|s| !s.is_empty())?;
    let description = card
        .proplist
        .get_str("device.description")
        .or_else(|| card.proplist.get_str("alsa.card_name"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| name.to_owned());
    let active_profile = card
        .active_profile
        .as_ref()
        .and_then(|p| p.name.as_deref())
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let mut profiles: Vec<AudioProfile> = card
        .profiles
        .iter()
        .filter_map(|p| {
            let prof_name = p.name.as_deref().filter(|s| !s.is_empty())?;
            let prof_desc = p
                .description
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(prof_name);
            Some(AudioProfile {
                index: 0,
                name: prof_name.to_owned(),
                description: prof_desc.to_owned(),
                priority: p.priority,
                available: p.available,
            })
        })
        .collect();
    profiles.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.name.cmp(&b.name))
    });
    Some(AudioCard {
        id: card.index,
        name: name.to_owned(),
        description,
        active_profile,
        profiles,
    })
}

#[allow(clippy::too_many_arguments)]
fn pulse_stream_record(
    id: u32,
    direction: AudioDirection,
    proplist: &pa::proplist::Proplist,
    stream_name: Option<&str>,
    target_id: u32,
    corked: bool,
    volume: &ChannelVolumes,
    raw_channel_map: &pa::channelmap::Map,
    muted: bool,
) -> PulseStreamRecord {
    let meta = StreamMetadata::extract(stream_name, |k| proplist.get_str(k));
    PulseStreamRecord {
        id,
        direction,
        name: meta.name,
        app_name: meta.app_name,
        binary_name: meta.binary_name,
        media_role: meta.media_role,
        target_id,
        corked,
        vol: PulseVolumeState::from_pa(volume, raw_channel_map, muted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_channel_volumes_roundtrips_asymmetric_stereo() {
        let cv = pulse_channel_volumes_from_slice(&[0.8, 0.4]);
        let (peak, channels, _db) = pulse_volume_parts(&cv);
        assert!((peak - 0.8).abs() < 1e-3);
        assert_eq!(channels.len(), 2);
        assert!((channels[0] - 0.8).abs() < 1e-3);
        assert!((channels[1] - 0.4).abs() < 1e-3);
    }

    #[test]
    fn merge_nonzero_template_preserves_ratio_across_zero_volume() {
        let tpl = merge_nonzero_template(Some(vec![0.8, 0.4]), &[0.0, 0.0]);
        assert_eq!(tpl, vec![0.8, 0.4]);
        let updated = merge_nonzero_template(Some(tpl), &[0.6, 0.3]);
        assert_eq!(updated, vec![0.6, 0.3]);
    }
}
