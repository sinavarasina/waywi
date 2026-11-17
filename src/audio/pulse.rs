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

/// Device record shared by output sinks and input sources (mics).
#[derive(Debug, Clone)]
struct PulseDeviceRecord {
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

/// Device and per-app stream state owned by the PulseAudio worker; rebuilt from introspection
/// batches and re-published as a snapshot whenever any part changes.
#[derive(Default)]
struct PulseRecordState {
    default_sink_name: Option<String>,
    default_source_name: Option<String>,
    sinks: BTreeMap<u32, PulseDeviceRecord>,
    sources: BTreeMap<u32, PulseDeviceRecord>,
    sink_inputs: BTreeMap<u32, PulseStreamRecord>,
    source_outputs: BTreeMap<u32, PulseStreamRecord>,
}

impl PulseRecordState {
    fn publish(
        &self,
        shared: &Arc<Mutex<SharedAudioState>>,
        last_snapshot: &mut Option<AudioSnapshot>,
    ) {
        let devices: Vec<AudioDevice> = self
            .sinks
            .values()
            .map(|s| AudioDevice {
                id: s.id,
                name: s.name.clone(),
                description: s.description.clone(),
            })
            .collect();
        let sources: Vec<AudioDevice> = self.sources.values().map(rec_device).collect();
        let streams: Vec<AudioStream> = self
            .sink_inputs
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
        let captures: Vec<AudioCapture> = self
            .source_outputs
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

        let sink_target = resolve_target(self.default_sink_name.as_deref(), &self.sinks);
        let source_target = resolve_target(self.default_source_name.as_deref(), &self.sources);

        let snapshot = AudioSnapshot {
            linear: sink_target.as_ref().map_or(0.0, |s| s.volume_linear),
            db: sink_target.as_ref().and_then(|s| s.db),
            muted: sink_target.as_ref().is_some_and(|s| s.muted),
            default_sink: sink_target.as_ref().map(|s| rec_device(s)),
            devices,
            source_linear: source_target.as_ref().map_or(0.0, |s| s.volume_linear),
            source_db: source_target.as_ref().and_then(|s| s.db),
            source_muted: source_target.as_ref().is_some_and(|s| s.muted),
            default_source: source_target.as_ref().map(|s| rec_device(s)),
            sources,
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
                records.sinks = batch.into_iter().map(|rec| (rec.id, rec)).collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::SourceBatch { sources: batch } => {
                records.sources = batch.into_iter().map(|rec| (rec.id, rec)).collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::SinkInputBatch { inputs: batch } => {
                records.sink_inputs = batch.into_iter().map(|rec| (rec.id, rec)).collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::SourceOutputBatch { outputs: batch } => {
                records.source_outputs = batch.into_iter().map(|rec| (rec.id, rec)).collect();
                records.publish(&shared, &mut last_snapshot);
            }
            PulseWorkerMsg::Cmd(cmd) => {
                if !ready {
                    continue;
                }
                apply_pulse_command(&mut ml, &mut ctx, cmd, &records);
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
            if let Ok(mut list) = collected_cb.lock()
                && let Some(rec) = pulse_device_record(
                    sink.index,
                    sink.name.as_deref(),
                    sink.description.as_deref(),
                    &sink.volume,
                    sink.mute,
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
            if let Ok(mut list) = collected_sources_cb.lock()
                && let Some(rec) = pulse_device_record(
                    source.index,
                    source.name.as_deref(),
                    source.description.as_deref(),
                    &source.volume,
                    source.mute,
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
    records: &PulseRecordState,
) {
    let target = resolve_target(records.default_sink_name.as_deref(), &records.sinks);
    let source_target = resolve_target(records.default_source_name.as_deref(), &records.sources);
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
            if let Some(sink) = records.sinks.get(&id) {
                let _op = ctx.set_default_sink(&sink.name, |_| {});
            }
        }
        AudioCommand::SetSourceVolumeLinear(linear) => {
            if let Some(source) = source_target {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_volume_by_name(
                    &source.name,
                    &pulse_channel_volumes(source.channels, linear),
                    None,
                );
            }
        }
        AudioCommand::SetSourceMuted(muted) => {
            if let Some(source) = source_target {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_mute_by_name(&source.name, muted, None);
            }
        }
        AudioCommand::ToggleSourceMute => {
            if let Some(source) = source_target {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_mute_by_name(&source.name, !source.muted, None);
            }
        }
        AudioCommand::SelectSource(id) => {
            if let Some(source) = records.sources.get(&id) {
                let _op = ctx.set_default_source(&source.name, |_| {});
            }
        }
        AudioCommand::SetStreamVolumeLinear(id, linear) => {
            if let Some(rec) = records.sink_inputs.get(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_input_volume(
                    id,
                    &pulse_channel_volumes(rec.channels, linear),
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
            if let Some(rec) = records.source_outputs.get(&id) {
                let mut introspect = ctx.introspect();
                let _op = introspect.set_source_output_volume(
                    id,
                    &pulse_channel_volumes(rec.channels, linear),
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
fn pulse_channel_volumes(channels: u8, linear: f32) -> ChannelVolumes {
    let mut cv = ChannelVolumes::default();
    let vol = Volume::from(VolumeLinear(f64::from(linear.clamp(0.0, 1.5))));
    cv.set(channels.max(1), vol);
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
    }
}

fn pulse_volume_parts(volume: &ChannelVolumes) -> (f32, Option<f32>) {
    let avg = volume.avg();
    let raw_db = VolumeDB::from(avg).0 as f32;
    (
        VolumeLinear::from(avg).0.max(0.0) as f32,
        raw_db.is_finite().then_some(raw_db),
    )
}

/// Builds a device record; `None` when the server reports no usable name.
fn pulse_device_record(
    id: u32,
    name: Option<&str>,
    description: Option<&str>,
    volume: &ChannelVolumes,
    muted: bool,
) -> Option<PulseDeviceRecord> {
    let name = name.filter(|s| !s.is_empty())?;
    let description = description
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_owned();
    let (volume_linear, db) = pulse_volume_parts(volume);
    Some(PulseDeviceRecord {
        id,
        name: name.to_owned(),
        description,
        channels: volume.len().max(1),
        volume_linear,
        db,
        muted,
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
