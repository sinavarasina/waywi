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
use super::types::{AudioDevice, AudioEvent, AudioSnapshot};

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

enum PulseWorkerMsg {
    StateChanged,
    Refresh,
    ServerInfo(Option<String>),
    SinkBatch { sinks: Vec<PulseSinkRecord> },
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
                            let _op = ctx
                                .subscribe(InterestMaskSet::SINK | InterestMaskSet::SERVER, |_| {});
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
                publish_pulse_snapshot(&shared, &mut last_snapshot, &default_sink_name, &sinks);
            }
            PulseWorkerMsg::SinkBatch { sinks: batch } => {
                sinks.clear();
                for rec in batch {
                    sinks.insert(rec.id, rec);
                }
                publish_pulse_snapshot(&shared, &mut last_snapshot, &default_sink_name, &sinks);
            }
            PulseWorkerMsg::Cmd(cmd) => {
                if !ready {
                    continue;
                }
                apply_pulse_command(&mut ml, &mut ctx, cmd, &default_sink_name, &sinks);
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
            let avg = sink.volume.avg();
            let linear = VolumeLinear::from(avg).0.max(0.0) as f32;
            let raw_db = VolumeDB::from(avg).0 as f32;
            let db = if raw_db.is_finite() {
                Some(raw_db)
            } else {
                None
            };
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

    ml.unlock();
}

fn apply_pulse_command(
    ml: &mut Mainloop,
    ctx: &mut Context,
    cmd: AudioCommand,
    default_sink_name: &Option<String>,
    sinks: &BTreeMap<u32, PulseSinkRecord>,
) {
    let target = resolve_target_sink(default_sink_name.as_deref(), sinks);
    ml.lock();
    match cmd {
        AudioCommand::SetVolumeLinear(linear) => {
            if let Some(sink) = target {
                let mut cv = ChannelVolumes::default();
                let vol = Volume::from(VolumeLinear(f64::from(linear.clamp(0.0, 1.5))));
                cv.set(sink.channels.max(1), vol);
                let mut introspect = ctx.introspect();
                let _op = introspect.set_sink_volume_by_name(&sink.name, &cv, None);
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
        AudioCommand::SetSourceVolumeLinear(_)
        | AudioCommand::SetSourceMuted(_)
        | AudioCommand::ToggleSourceMute
        | AudioCommand::SelectSource(_)
        | AudioCommand::Shutdown => {}
    }
    ml.unlock();
}

fn resolve_target_sink<'a>(
    default_sink_name: Option<&str>,
    sinks: &'a BTreeMap<u32, PulseSinkRecord>,
) -> Option<&'a PulseSinkRecord> {
    default_sink_name
        .and_then(|name| sinks.values().find(|s| s.name == name))
        .or_else(|| sinks.values().next())
}

fn publish_pulse_snapshot(
    shared: &Arc<Mutex<SharedAudioState>>,
    last_snapshot: &mut Option<AudioSnapshot>,
    default_sink_name: &Option<String>,
    sinks: &BTreeMap<u32, PulseSinkRecord>,
) {
    let devices: Vec<AudioDevice> = sinks
        .values()
        .map(|s| AudioDevice {
            id: s.id,
            name: s.name.clone(),
            description: s.description.clone(),
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
            ..AudioSnapshot::default()
        }
    } else {
        AudioSnapshot {
            devices,
            ..AudioSnapshot::default()
        }
    };

    update_snapshot(shared, last_snapshot, snapshot);
}
