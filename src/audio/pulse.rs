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
use super::types::{AudioCapture, AudioDevice, AudioEvent, AudioSnapshot, AudioStream};

#[derive(Debug, Clone)]
struct PulseSinkRecord {
    id: u32,
    name: String,
    description: String,
    channels: u8,
    volume_linear: f32,
    db: Option<f32>,
    muted: bool,
}

/// Per-application stream record shared by sink inputs (playback) and source outputs (capture);
/// `target_id` is the sink / source index the stream is attached to.
#[derive(Debug, Clone)]
struct PulseStreamRecord {
    id: u32,
    name: String,
    binary: Option<String>,
    target_id: u32,
    channels: u8,
    volume_linear: f32,
    db: Option<f32>,
    muted: bool,
}

enum PulseWorkerMsg {
    StateChanged,
    Refresh,
    ServerInfo(Option<String>),
    SinkBatch { sinks: Vec<PulseSinkRecord> },
    SinkInputBatch { inputs: Vec<PulseStreamRecord> },
    SourceOutputBatch { outputs: Vec<PulseStreamRecord> },
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
    let mut default_sink_name: Option<String> = None;
    let mut sinks: BTreeMap<u32, PulseSinkRecord> = BTreeMap::new();
    let mut sink_inputs: BTreeMap<u32, PulseStreamRecord> = BTreeMap::new();
    let mut source_outputs: BTreeMap<u32, PulseStreamRecord> = BTreeMap::new();

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
            PulseWorkerMsg::ServerInfo(name) => {
                default_sink_name = name;
                publish_pulse_snapshot(
                    &shared,
                    &mut last_snapshot,
                    &default_sink_name,
                    &sinks,
                    &sink_inputs,
                    &source_outputs,
                );
            }
            PulseWorkerMsg::SinkBatch { sinks: batch } => {
                sinks.clear();
                for rec in batch {
                    sinks.insert(rec.id, rec);
                }
                publish_pulse_snapshot(
                    &shared,
                    &mut last_snapshot,
                    &default_sink_name,
                    &sinks,
                    &sink_inputs,
                    &source_outputs,
                );
            }
            PulseWorkerMsg::SinkInputBatch { inputs: batch } => {
                sink_inputs.clear();
                for rec in batch {
                    sink_inputs.insert(rec.id, rec);
                }
                publish_pulse_snapshot(
                    &shared,
                    &mut last_snapshot,
                    &default_sink_name,
                    &sinks,
                    &sink_inputs,
                    &source_outputs,
                );
            }
            PulseWorkerMsg::SourceOutputBatch { outputs: batch } => {
                source_outputs.clear();
                for rec in batch {
                    source_outputs.insert(rec.id, rec);
                }
                publish_pulse_snapshot(
                    &shared,
                    &mut last_snapshot,
                    &default_sink_name,
                    &sinks,
                    &sink_inputs,
                    &source_outputs,
                );
            }
            PulseWorkerMsg::Cmd(cmd) => {
                if !ready {
                    continue;
                }
                apply_pulse_command(
                    &mut ml,
                    &mut ctx,
                    cmd,
                    &default_sink_name,
                    &sinks,
                    &sink_inputs,
                    &source_outputs,
                );
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
        let name = info.default_sink_name.as_deref().map(str::to_owned);
        let _ = tx_srv.send(PulseWorkerMsg::ServerInfo(name));
    });

    let collected = Arc::new(Mutex::new(Vec::<PulseSinkRecord>::new()));
    let collected_cb = Arc::clone(&collected);
    let tx_sinks = tx.clone();
    let _sink_op = introspect.get_sink_info_list(move |res| match res {
        ListResult::Item(sink) => {
            let Some(name) = sink.name.as_deref().filter(|s| !s.is_empty()) else {
                return;
            };
            let description = sink
                .description
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(name)
                .to_owned();
            let (linear, db) = pulse_volume_parts(&sink.volume);
            if let Ok(mut list) = collected_cb.lock() {
                list.push(PulseSinkRecord {
                    id: sink.index,
                    name: name.to_owned(),
                    description,
                    channels: sink.volume.len().max(1),
                    volume_linear: linear,
                    db,
                    muted: sink.mute,
                });
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
    default_sink_name: &Option<String>,
    sinks: &BTreeMap<u32, PulseSinkRecord>,
    sink_inputs: &BTreeMap<u32, PulseStreamRecord>,
    source_outputs: &BTreeMap<u32, PulseStreamRecord>,
) {
    let target = resolve_target_sink(default_sink_name.as_deref(), sinks);
    ml.lock();
    match cmd {
        AudioCommand::SetVolumeLinear(linear) => {
            if let Some(sink) = target {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_volume_by_name(
                    &sink.name,
                    &pulse_channel_volumes(sink.channels, linear),
                    None,
                );
            }
        }
        AudioCommand::SetMuted(muted) => {
            if let Some(sink) = target {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_mute_by_name(&sink.name, muted, None);
            }
        }
        AudioCommand::ToggleMute => {
            if let Some(sink) = target {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_mute_by_name(&sink.name, !sink.muted, None);
            }
        }
        AudioCommand::SelectDevice(id) => {
            if let Some(sink) = sinks.get(&id) {
                let _op = ctx.set_default_sink(&sink.name, |_| {});
            }
        }
        AudioCommand::SetStreamVolumeLinear(id, linear) => {
            if let Some(rec) = sink_inputs.get(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_input_volume(
                    id,
                    &pulse_channel_volumes(rec.channels, linear),
                    None,
                );
            }
        }
        AudioCommand::SetStreamMuted(id, muted) => {
            if sink_inputs.contains_key(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_input_mute(id, muted, None);
            }
        }
        AudioCommand::ToggleStreamMute(id) => {
            if let Some(rec) = sink_inputs.get(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_input_mute(id, !rec.muted, None);
            }
        }
        AudioCommand::SetCaptureVolumeLinear(id, linear) => {
            if let Some(rec) = source_outputs.get(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_output_volume(
                    id,
                    &pulse_channel_volumes(rec.channels, linear),
                    None,
                );
            }
        }
        AudioCommand::SetCaptureMuted(id, muted) => {
            if source_outputs.contains_key(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_output_mute(id, muted, None);
            }
        }
        AudioCommand::ToggleCaptureMute(id) => {
            if let Some(rec) = source_outputs.get(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_output_mute(id, !rec.muted, None);
            }
        }
        AudioCommand::SetSourceVolumeLinear(_)
        | AudioCommand::SetSourceMuted(_)
        | AudioCommand::ToggleSourceMute
        | AudioCommand::SelectSource(_)
        | AudioCommand::Shutdown => {}
    }
    ml.unlock();
}

fn pulse_channel_volumes(channels: u8, linear: f32) -> ChannelVolumes {
    let mut cv = ChannelVolumes::default();
    let vol = Volume::from(VolumeLinear(f64::from(linear.clamp(0.0, 1.5))));
    cv.set(channels.max(1), vol);
    cv
}

fn resolve_target_sink<'a>(
    default_sink_name: Option<&str>,
    sinks: &'a BTreeMap<u32, PulseSinkRecord>,
) -> Option<&'a PulseSinkRecord> {
    default_sink_name
        .and_then(|name| sinks.values().find(|s| s.name == name))
        .or_else(|| sinks.values().next())
}

fn pulse_volume_parts(volume: &ChannelVolumes) -> (f32, Option<f32>) {
    let avg = volume.avg();
    let raw_db = VolumeDB::from(avg).0 as f32;
    (
        VolumeLinear::from(avg).0.max(0.0) as f32,
        raw_db.is_finite().then_some(raw_db),
    )
}

fn pulse_stream_record(
    id: u32,
    proplist: &pa::proplist::Proplist,
    stream_name: Option<&str>,
    target_id: u32,
    volume: &ChannelVolumes,
    muted: bool,
) -> PulseStreamRecord {
    let app_name = proplist.get_str("application.name");
    let binary = proplist.get_str("application.process.binary");
    let (volume_linear, db) = pulse_volume_parts(volume);
    PulseStreamRecord {
        id,
        name: super::backend::stream_display_name(
            app_name.as_deref(),
            binary.as_deref(),
            stream_name,
        ),
        binary,
        target_id,
        channels: volume.len().max(1),
        volume_linear,
        db,
        muted,
    }
}

fn publish_pulse_snapshot(
    shared: &Arc<Mutex<SharedAudioState>>,
    last_snapshot: &mut Option<AudioSnapshot>,
    default_sink_name: &Option<String>,
    sinks: &BTreeMap<u32, PulseSinkRecord>,
    sink_inputs: &BTreeMap<u32, PulseStreamRecord>,
    source_outputs: &BTreeMap<u32, PulseStreamRecord>,
) {
    let devices: Vec<AudioDevice> = sinks
        .values()
        .map(|s| AudioDevice {
            id: s.id,
            name: s.name.clone(),
            description: s.description.clone(),
        })
        .collect();

    let streams: Vec<AudioStream> = sink_inputs
        .values()
        .map(|rec| AudioStream {
            id: rec.id,
            name: rec.name.clone(),
            binary: rec.binary.clone(),
            sink_id: Some(rec.target_id),
            linear: rec.volume_linear,
            db: rec.db,
            muted: rec.muted,
        })
        .collect();
    let captures: Vec<AudioCapture> = source_outputs
        .values()
        .map(|rec| AudioCapture {
            id: rec.id,
            name: rec.name.clone(),
            binary: rec.binary.clone(),
            source_id: Some(rec.target_id),
            linear: rec.volume_linear,
            db: rec.db,
            muted: rec.muted,
        })
        .collect();

    let target = resolve_target_sink(default_sink_name.as_deref(), sinks);
    let snapshot = if let Some(sink) = target {
        AudioSnapshot {
            linear: sink.volume_linear,
            db: sink.db,
            muted: sink.muted,
            default_sink: Some(AudioDevice {
                id: sink.id,
                name: sink.name.clone(),
                description: sink.description.clone(),
            }),
            devices,
            streams,
            captures,
            ..AudioSnapshot::default()
        }
    } else {
        AudioSnapshot {
            devices,
            streams,
            captures,
            ..AudioSnapshot::default()
        }
    };

    update_snapshot(shared, last_snapshot, snapshot);
}
