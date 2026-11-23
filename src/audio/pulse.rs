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
    AudioBackend, AudioCommand, AudioCommandSender, SharedAudioState, update_snapshot,
};
use super::types::{
    AudioCapture, AudioCard, AudioDevice, AudioEvent, AudioPort, AudioPortAvailability,
    AudioProfile, AudioSnapshot, AudioStream, apply_linear_balance, sanitize_linear_channels,
    scale_linear_channels,
};

/// Device record shared by output sinks and input sources (mics).
#[derive(Debug, Clone)]
struct PulseDeviceRecord {
    id: u32,
    name: String,
    description: String,
    channels: u8,
    volume_linear: f32,
    channel_volumes: Vec<f32>,
    nonzero_template: Vec<f32>,
    db: Option<f32>,
    muted: bool,
    card_id: Option<u32>,
    active_port: Option<String>,
    ports: Vec<AudioPort>,
}

/// Per-application stream record shared by sink inputs (playback) and source outputs (capture);
/// `target_id` is the sink / source index the stream is attached to.
#[derive(Debug, Clone)]
struct PulseStreamRecord {
    id: u32,
    name: String,
    binary: Option<String>,
    icon_name: Option<String>,
    app_id: Option<String>,
    target_id: u32,
    channels: u8,
    volume_linear: f32,
    channel_volumes: Vec<f32>,
    nonzero_template: Vec<f32>,
    db: Option<f32>,
    muted: bool,
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
    fn publish(
        &self,
        shared: &Arc<Mutex<SharedAudioState>>,
        last_snapshot: &mut Option<AudioSnapshot>,
    ) {
        let devices: Vec<AudioDevice> = self.sinks.values().map(rec_device).collect();
        let sources: Vec<AudioDevice> = self.sources.values().map(rec_device).collect();
        let cards: Vec<AudioCard> = self.cards.values().cloned().collect();
        let streams: Vec<AudioStream> = self
            .sink_inputs
            .values()
            .map(|rec| AudioStream {
                id: rec.id,
                name: rec.name.clone(),
                binary: rec.binary.clone(),
                icon_name: rec.icon_name.clone(),
                app_id: rec.app_id.clone(),
                sink_id: Some(rec.target_id),
                linear: rec.volume_linear,
                channels: rec.channel_volumes.clone(),
                db: rec.db,
                muted: rec.muted,
            })
            .collect();
        let captures: Vec<AudioCapture> = self
            .source_outputs
            .values()
            .map(|rec| AudioCapture {
                id: rec.id,
                name: rec.name.clone(),
                binary: rec.binary.clone(),
                icon_name: rec.icon_name.clone(),
                app_id: rec.app_id.clone(),
                source_id: Some(rec.target_id),
                linear: rec.volume_linear,
                channels: rec.channel_volumes.clone(),
                db: rec.db,
                muted: rec.muted,
            })
            .collect();

        let sink_target = resolve_target(self.default_sink_name.as_deref(), &self.sinks);
        let source_target = resolve_target(self.default_source_name.as_deref(), &self.sources);

        let snapshot = AudioSnapshot {
            linear: sink_target.as_ref().map_or(0.0, |s| s.volume_linear),
            channels: sink_target
                .as_ref()
                .map_or_else(Vec::new, |s| s.channel_volumes.clone()),
            db: sink_target.as_ref().and_then(|s| s.db),
            muted: sink_target.as_ref().is_some_and(|s| s.muted),
            default_sink: sink_target.as_ref().map(|s| rec_device(s)),
            devices,
            source_linear: source_target.as_ref().map_or(0.0, |s| s.volume_linear),
            source_channels: source_target
                .as_ref()
                .map_or_else(Vec::new, |s| s.channel_volumes.clone()),
            source_db: source_target.as_ref().and_then(|s| s.db),
            source_muted: source_target.as_ref().is_some_and(|s| s.muted),
            default_source: source_target.as_ref().map(|s| rec_device(s)),
            sources,
            cards,
            streams,
            captures,
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
    SinkBatch {
        sinks: Vec<PulseDeviceRecord>,
    },
    SourceBatch {
        sources: Vec<PulseDeviceRecord>,
    },
    CardBatch {
        cards: Vec<AudioCard>,
    },
    SinkInputBatch {
        inputs: Vec<PulseStreamRecord>,
    },
    SourceOutputBatch {
        outputs: Vec<PulseStreamRecord>,
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
    let mut last_snapshot: Option<AudioSnapshot> = None;
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
                            guard.push_event(AudioEvent::Reconnected);
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
                            guard.push_event(AudioEvent::Disconnected);
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
            PulseWorkerMsg::SinkBatch { sinks: batch } => {
                let mut prev = std::mem::take(&mut records.sinks);
                records.sinks = batch
                    .into_iter()
                    .map(|mut rec| {
                        let prev_tpl = prev.remove(&rec.id).map(|p| p.nonzero_template);
                        rec.nonzero_template =
                            merge_nonzero_template(prev_tpl, &rec.channel_volumes);
                        (rec.id, rec)
                    })
                    .collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::SourceBatch { sources: batch } => {
                let mut prev = std::mem::take(&mut records.sources);
                records.sources = batch
                    .into_iter()
                    .map(|mut rec| {
                        let prev_tpl = prev.remove(&rec.id).map(|p| p.nonzero_template);
                        rec.nonzero_template =
                            merge_nonzero_template(prev_tpl, &rec.channel_volumes);
                        (rec.id, rec)
                    })
                    .collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::CardBatch { cards: batch } => {
                records.cards = batch.into_iter().map(|c| (c.id, c)).collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::SinkInputBatch { inputs: batch } => {
                let mut prev = std::mem::take(&mut records.sink_inputs);
                records.sink_inputs = batch
                    .into_iter()
                    .map(|mut rec| {
                        let prev_tpl = prev.remove(&rec.id).map(|p| p.nonzero_template);
                        rec.nonzero_template =
                            merge_nonzero_template(prev_tpl, &rec.channel_volumes);
                        (rec.id, rec)
                    })
                    .collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::SourceOutputBatch { outputs: batch } => {
                let mut prev = std::mem::take(&mut records.source_outputs);
                records.source_outputs = batch
                    .into_iter()
                    .map(|mut rec| {
                        let prev_tpl = prev.remove(&rec.id).map(|p| p.nonzero_template);
                        rec.nonzero_template =
                            merge_nonzero_template(prev_tpl, &rec.channel_volumes);
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

    let collected = Arc::new(Mutex::new(Vec::<PulseDeviceRecord>::new()));
    let collected_cb = Arc::clone(&collected);
    let tx_sinks = tx.clone();
    let _sink_op = introspect.get_sink_info_list(move |res| match res {
        ListResult::Item(sink) => {
            let ports: Vec<AudioPort> = sink
                .ports
                .iter()
                .filter_map(|p| {
                    let port_name = p.name.as_deref().filter(|s| !s.is_empty())?;
                    let port_desc = p
                        .description
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(port_name);
                    Some(AudioPort {
                        index: 0,
                        name: port_name.to_owned(),
                        description: port_desc.to_owned(),
                        priority: p.priority,
                        availability: pulse_port_availability(p.available),
                    })
                })
                .collect();
            if let Ok(mut list) = collected_cb.lock()
                && let Some(rec) = pulse_device_record(
                    sink.index,
                    sink.name.as_deref(),
                    sink.description.as_deref(),
                    &sink.volume,
                    sink.mute,
                    sink.card,
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
            if let Ok(mut list) = collected_cb.lock() {
                let sinks = std::mem::take(&mut *list);
                let _ = tx_sinks.send(PulseWorkerMsg::SinkBatch { sinks });
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
            let ports: Vec<AudioPort> = source
                .ports
                .iter()
                .filter_map(|p| {
                    let port_name = p.name.as_deref().filter(|s| !s.is_empty())?;
                    let port_desc = p
                        .description
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(port_name);
                    Some(AudioPort {
                        index: 0,
                        name: port_name.to_owned(),
                        description: port_desc.to_owned(),
                        priority: p.priority,
                        availability: pulse_port_availability(p.available),
                    })
                })
                .collect();
            if let Ok(mut list) = collected_sources_cb.lock()
                && let Some(rec) = pulse_device_record(
                    source.index,
                    source.name.as_deref(),
                    source.description.as_deref(),
                    &source.volume,
                    source.mute,
                    source.card,
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
                let sources = std::mem::take(&mut *list);
                let _ = tx_sources.send(PulseWorkerMsg::SourceBatch { sources });
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
            if let Ok(mut list) = collected_inputs_cb.lock() {
                list.push(pulse_stream_record(
                    info.index,
                    &info.proplist,
                    info.name.as_deref(),
                    info.sink,
                    &info.volume,
                    info.mute,
                ));
            }
        }
        ListResult::End => {
            if let Ok(mut list) = collected_inputs_cb.lock() {
                let inputs = std::mem::take(&mut *list);
                let _ = tx_inputs.send(PulseWorkerMsg::SinkInputBatch { inputs });
            }
        }
        ListResult::Error => {}
    });

    let collected_outputs = Arc::new(Mutex::new(Vec::<PulseStreamRecord>::new()));
    let collected_outputs_cb = Arc::clone(&collected_outputs);
    let tx_outputs = tx.clone();
    let _output_op = introspect.get_source_output_info_list(move |res| match res {
        ListResult::Item(info) => {
            if let Ok(mut list) = collected_outputs_cb.lock() {
                list.push(pulse_stream_record(
                    info.index,
                    &info.proplist,
                    info.name.as_deref(),
                    info.source,
                    &info.volume,
                    info.mute,
                ));
            }
        }
        ListResult::End => {
            if let Ok(mut list) = collected_outputs_cb.lock() {
                let outputs = std::mem::take(&mut *list);
                let _ = tx_outputs.send(PulseWorkerMsg::SourceOutputBatch { outputs });
            }
        }
        ListResult::Error => {}
    });

    ml.unlock();
}

fn apply_pulse_command(
    ml: &mut Mainloop,
    ctx: &mut Context,
    cmd: AudioCommand,
    records: &mut PulseRecordState,
) {
    let target_id =
        resolve_target(records.default_sink_name.as_deref(), &records.sinks).map(|s| s.id);
    let source_target_id =
        resolve_target(records.default_source_name.as_deref(), &records.sources).map(|s| s.id);
    ml.lock();
    match cmd {
        AudioCommand::SetVolumeLinear(linear) => {
            if let Some(id) = target_id
                && let Some(sink) = records.sinks.get_mut(&id)
            {
                let scaled = scale_linear_channels(
                    &sink.channel_volumes,
                    &sink.nonzero_template,
                    sink.channels as usize,
                    linear,
                );
                update_device_channels_optimistic(sink, &scaled);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_volume_by_name(
                    &sink.name,
                    &pulse_channel_volumes_from_slice(&scaled),
                    None,
                );
            }
        }
        AudioCommand::SetBalance(balance) => {
            if let Some(id) = target_id
                && let Some(sink) = records.sinks.get_mut(&id)
            {
                let balanced = apply_linear_balance(
                    &sink.channel_volumes,
                    sink.channels as usize,
                    sink.volume_linear,
                    balance,
                );
                update_device_channels_optimistic(sink, &balanced);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_volume_by_name(
                    &sink.name,
                    &pulse_channel_volumes_from_slice(&balanced),
                    None,
                );
            }
        }
        AudioCommand::SetChannelVolumes(channels) => {
            if let Some(channels) = sanitize_linear_channels(&channels)
                && let Some(id) = target_id
                && let Some(sink) = records.sinks.get_mut(&id)
            {
                update_device_channels_optimistic(sink, &channels);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_volume_by_name(
                    &sink.name,
                    &pulse_channel_volumes_from_slice(&channels),
                    None,
                );
            }
        }
        AudioCommand::SetMuted(muted) => {
            if let Some(id) = target_id
                && let Some(sink) = records.sinks.get(&id)
            {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_mute_by_name(&sink.name, muted, None);
            }
        }
        AudioCommand::ToggleMute => {
            if let Some(id) = target_id
                && let Some(sink) = records.sinks.get(&id)
            {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_mute_by_name(&sink.name, !sink.muted, None);
            }
        }
        AudioCommand::SelectDevice(id) => {
            if let Some(sink) = records.sinks.get(&id)
                && is_valid_pa_str(&sink.name)
            {
                let _op = ctx.set_default_sink(&sink.name, |_| {});
            }
        }
        AudioCommand::SelectPort(device_id, port_name) => {
            let id = device_id.or(target_id);
            if is_valid_pa_str(&port_name)
                && let Some(id) = id
                && let Some(sink) = records.sinks.get(&id)
                && is_valid_pa_str(&sink.name)
            {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_port_by_name(&sink.name, &port_name, None);
            }
        }
        AudioCommand::SetSourceVolumeLinear(linear) => {
            if let Some(id) = source_target_id
                && let Some(source) = records.sources.get_mut(&id)
            {
                let scaled = scale_linear_channels(
                    &source.channel_volumes,
                    &source.nonzero_template,
                    source.channels as usize,
                    linear,
                );
                update_device_channels_optimistic(source, &scaled);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_volume_by_name(
                    &source.name,
                    &pulse_channel_volumes_from_slice(&scaled),
                    None,
                );
            }
        }
        AudioCommand::SetSourceBalance(balance) => {
            if let Some(id) = source_target_id
                && let Some(source) = records.sources.get_mut(&id)
            {
                let balanced = apply_linear_balance(
                    &source.channel_volumes,
                    source.channels as usize,
                    source.volume_linear,
                    balance,
                );
                update_device_channels_optimistic(source, &balanced);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_volume_by_name(
                    &source.name,
                    &pulse_channel_volumes_from_slice(&balanced),
                    None,
                );
            }
        }
        AudioCommand::SetSourceChannelVolumes(channels) => {
            if let Some(channels) = sanitize_linear_channels(&channels)
                && let Some(id) = source_target_id
                && let Some(source) = records.sources.get_mut(&id)
            {
                update_device_channels_optimistic(source, &channels);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_volume_by_name(
                    &source.name,
                    &pulse_channel_volumes_from_slice(&channels),
                    None,
                );
            }
        }
        AudioCommand::SetSourceMuted(muted) => {
            if let Some(id) = source_target_id
                && let Some(source) = records.sources.get(&id)
            {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_mute_by_name(&source.name, muted, None);
            }
        }
        AudioCommand::ToggleSourceMute => {
            if let Some(id) = source_target_id
                && let Some(source) = records.sources.get(&id)
            {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_mute_by_name(&source.name, !source.muted, None);
            }
        }
        AudioCommand::SelectSource(id) => {
            if let Some(source) = records.sources.get(&id)
                && is_valid_pa_str(&source.name)
            {
                let _op = ctx.set_default_source(&source.name, |_| {});
            }
        }
        AudioCommand::SelectSourcePort(device_id, port_name) => {
            let id = device_id.or(source_target_id);
            if is_valid_pa_str(&port_name)
                && let Some(id) = id
                && let Some(source) = records.sources.get(&id)
                && is_valid_pa_str(&source.name)
            {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_port_by_name(&source.name, &port_name, None);
            }
        }
        AudioCommand::SelectCardProfile(card_id, profile_name) => {
            if is_valid_pa_str(&profile_name) && records.cards.contains_key(&card_id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_card_profile_by_index(card_id, &profile_name, None);
            }
        }
        AudioCommand::SetStreamVolumeLinear(id, linear) => {
            if let Some(rec) = records.sink_inputs.get_mut(&id) {
                let scaled = scale_linear_channels(
                    &rec.channel_volumes,
                    &rec.nonzero_template,
                    rec.channels as usize,
                    linear,
                );
                update_stream_channels_optimistic(rec, &scaled);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_input_volume(
                    id,
                    &pulse_channel_volumes_from_slice(&scaled),
                    None,
                );
            }
        }
        AudioCommand::SetStreamBalance(id, balance) => {
            if let Some(rec) = records.sink_inputs.get_mut(&id) {
                let balanced = apply_linear_balance(
                    &rec.channel_volumes,
                    rec.channels as usize,
                    rec.volume_linear,
                    balance,
                );
                update_stream_channels_optimistic(rec, &balanced);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_input_volume(
                    id,
                    &pulse_channel_volumes_from_slice(&balanced),
                    None,
                );
            }
        }
        AudioCommand::SetStreamChannelVolumes(id, channels) => {
            if let Some(channels) = sanitize_linear_channels(&channels)
                && let Some(rec) = records.sink_inputs.get_mut(&id)
            {
                update_stream_channels_optimistic(rec, &channels);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_input_volume(
                    id,
                    &pulse_channel_volumes_from_slice(&channels),
                    None,
                );
            }
        }
        AudioCommand::SetStreamMuted(id, muted) => {
            if records.sink_inputs.contains_key(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_input_mute(id, muted, None);
            }
        }
        AudioCommand::ToggleStreamMute(id) => {
            if let Some(rec) = records.sink_inputs.get(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_input_mute(id, !rec.muted, None);
            }
        }
        AudioCommand::SetCaptureVolumeLinear(id, linear) => {
            if let Some(rec) = records.source_outputs.get_mut(&id) {
                let scaled = scale_linear_channels(
                    &rec.channel_volumes,
                    &rec.nonzero_template,
                    rec.channels as usize,
                    linear,
                );
                update_stream_channels_optimistic(rec, &scaled);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_output_volume(
                    id,
                    &pulse_channel_volumes_from_slice(&scaled),
                    None,
                );
            }
        }
        AudioCommand::SetCaptureBalance(id, balance) => {
            if let Some(rec) = records.source_outputs.get_mut(&id) {
                let balanced = apply_linear_balance(
                    &rec.channel_volumes,
                    rec.channels as usize,
                    rec.volume_linear,
                    balance,
                );
                update_stream_channels_optimistic(rec, &balanced);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_output_volume(
                    id,
                    &pulse_channel_volumes_from_slice(&balanced),
                    None,
                );
            }
        }
        AudioCommand::SetCaptureChannelVolumes(id, channels) => {
            if let Some(channels) = sanitize_linear_channels(&channels)
                && let Some(rec) = records.source_outputs.get_mut(&id)
            {
                update_stream_channels_optimistic(rec, &channels);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_output_volume(
                    id,
                    &pulse_channel_volumes_from_slice(&channels),
                    None,
                );
            }
        }
        AudioCommand::SetCaptureMuted(id, muted) => {
            if records.source_outputs.contains_key(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_output_mute(id, muted, None);
            }
        }
        AudioCommand::ToggleCaptureMute(id) => {
            if let Some(rec) = records.source_outputs.get(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_output_mute(id, !rec.muted, None);
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

fn update_device_channels_optimistic(rec: &mut PulseDeviceRecord, channels: &[f32]) {
    if channels.is_empty() {
        return;
    }
    let peak = channels.iter().copied().fold(0.0_f32, f32::max);
    rec.volume_linear = peak;
    rec.channels = channels.len().min(u8::MAX as usize) as u8;
    rec.channel_volumes.clear();
    rec.channel_volumes.extend_from_slice(channels);
    if peak > 1e-6 {
        rec.nonzero_template.clear();
        rec.nonzero_template.extend_from_slice(channels);
    }
}

fn update_stream_channels_optimistic(rec: &mut PulseStreamRecord, channels: &[f32]) {
    if channels.is_empty() {
        return;
    }
    let peak = channels.iter().copied().fold(0.0_f32, f32::max);
    rec.volume_linear = peak;
    rec.channels = channels.len().min(u8::MAX as usize) as u8;
    rec.channel_volumes.clear();
    rec.channel_volumes.extend_from_slice(channels);
    if peak > 1e-6 {
        rec.nonzero_template.clear();
        rec.nonzero_template.extend_from_slice(channels);
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

fn rec_device(rec: &PulseDeviceRecord) -> AudioDevice {
    AudioDevice {
        id: rec.id,
        name: rec.name.clone(),
        description: rec.description.clone(),
        card_id: rec.card_id,
        active_port: rec.active_port.clone(),
        ports: rec.ports.clone(),
    }
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
    muted: bool,
    card_id: Option<u32>,
    active_port: Option<&str>,
    mut ports: Vec<AudioPort>,
) -> Option<PulseDeviceRecord> {
    let name = name.filter(|s| !s.is_empty())?;
    let description = description
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_owned();
    let (volume_linear, channel_volumes, db) = pulse_volume_parts(volume);
    ports.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.name.cmp(&b.name))
    });
    let nonzero_template = if channel_volumes.iter().any(|&v| v > 1e-6) {
        channel_volumes.clone()
    } else {
        vec![1.0; channel_volumes.len().max(1)]
    };
    Some(PulseDeviceRecord {
        id,
        name: name.to_owned(),
        description,
        channels: volume.len().max(1),
        volume_linear,
        channel_volumes,
        nonzero_template,
        db,
        muted,
        card_id,
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

fn pulse_stream_record(
    id: u32,
    proplist: &pa::proplist::Proplist,
    stream_name: Option<&str>,
    target_id: u32,
    volume: &ChannelVolumes,
    muted: bool,
) -> PulseStreamRecord {
    let meta = super::backend::StreamMetadata::extract(stream_name, |k| proplist.get_str(k));
    let (volume_linear, channel_volumes, db) = pulse_volume_parts(volume);
    let nonzero_template = if channel_volumes.iter().any(|&v| v > 1e-6) {
        channel_volumes.clone()
    } else {
        vec![1.0; channel_volumes.len().max(1)]
    };
    PulseStreamRecord {
        id,
        name: meta.name,
        binary: meta.binary,
        icon_name: meta.icon_name,
        app_id: meta.app_id,
        target_id,
        channels: volume.len().max(1),
        volume_linear,
        channel_volumes,
        nonzero_template,
        db,
        muted,
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
