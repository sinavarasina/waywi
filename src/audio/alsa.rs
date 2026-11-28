//! ALSA (`alsa` crate) mixer backend implementation.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use alsa::mixer::{Mixer, Selem, SelemChannelId, SelemId};
use alsa::poll::{Descriptors, poll, pollfd};

use crate::error::{Result, WaywiError};

use super::backend::{
    AudioBackend, AudioCommand, AudioCommandSender, SharedAudioState, update_snapshot,
};
use super::types::{
    AudioChannelPosition, AudioDevice, AudioDirection, AudioEvent, AudioSnapshot, AudioTarget,
    AudioVolume, apply_linear_balance_with_map, sanitize_linear_channels, scale_linear_channels,
};

const PREFERRED_OUTPUT_SELEMS: &[&str] = &["Master", "PCM", "Speaker", "Headphone", "DAC"];
const PREFERRED_INPUT_SELEMS: &[&str] = &[
    "Capture",
    "Mic",
    "Internal Mic",
    "Front Mic",
    "Headset Mic",
    "Input",
];

const ORDERED_ALSA_CHANNELS: &[(SelemChannelId, AudioChannelPosition)] = &[
    (SelemChannelId::FrontLeft, AudioChannelPosition::FrontLeft),
    (SelemChannelId::FrontRight, AudioChannelPosition::FrontRight),
    (
        SelemChannelId::FrontCenter,
        AudioChannelPosition::FrontCenter,
    ),
    (SelemChannelId::Woofer, AudioChannelPosition::Lfe),
    (SelemChannelId::RearLeft, AudioChannelPosition::RearLeft),
    (SelemChannelId::RearRight, AudioChannelPosition::RearRight),
    (SelemChannelId::RearCenter, AudioChannelPosition::RearCenter),
    (SelemChannelId::SideLeft, AudioChannelPosition::SideLeft),
    (SelemChannelId::SideRight, AudioChannelPosition::SideRight),
];

struct AlsaCommandSender {
    tx: mpsc::Sender<AudioCommand>,
    wake_tx: Mutex<UnixStream>,
}

impl AudioCommandSender for AlsaCommandSender {
    fn send(&self, cmd: AudioCommand) -> Result<()> {
        self.tx
            .send(cmd)
            .map_err(|_| WaywiError::AudioError("alsa worker channel disconnected".into()))?;
        if let Ok(mut sock) = self.wake_tx.lock() {
            let _ = sock.write_all(&[1]);
        }
        Ok(())
    }
}

/// ALSA [`AudioBackend`] driving an `alsa::mixer::Mixer` worker thread via zero-polling `poll()`.
pub(crate) struct AlsaBackend;

impl AudioBackend for AlsaBackend {
    fn start(
        &self,
        shared: Arc<Mutex<SharedAudioState>>,
        stop_flag: Arc<AtomicBool>,
    ) -> Option<(Box<dyn AudioCommandSender>, JoinHandle<()>)> {
        let (wake_tx, wake_rx) = UnixStream::pair().ok()?;
        let _ = wake_tx.set_nonblocking(true);
        let _ = wake_rx.set_nonblocking(true);

        let (tx, rx) = mpsc::channel::<AudioCommand>();
        let join = thread::Builder::new()
            .name("waywi-alsa".into())
            .spawn(move || run_alsa_worker(shared, stop_flag, rx, wake_rx))
            .ok()?;

        Some((
            Box::new(AlsaCommandSender {
                tx,
                wake_tx: Mutex::new(wake_tx),
            }),
            join,
        ))
    }
}

fn open_default_mixer() -> Option<(String, Mixer)> {
    for card in ["default", "hw:0"] {
        if let Ok(mixer) = Mixer::new(card, true) {
            return Some((card.to_owned(), mixer));
        }
    }
    None
}

fn run_alsa_worker(
    shared: Arc<Mutex<SharedAudioState>>,
    stop_flag: Arc<AtomicBool>,
    cmd_rx: mpsc::Receiver<AudioCommand>,
    mut wake_rx: UnixStream,
) {
    let Some((card_name, mixer)) = open_default_mixer() else {
        return;
    };

    if let Ok(mut guard) = shared.lock() {
        guard.connected = true;
        guard.push_event(AudioEvent::BackendReconnected);
    }

    let mut selected_output = pick_initial_selem(&mixer, AudioDirection::Output);
    let mut selected_input = pick_initial_selem(&mixer, AudioDirection::Input);
    let mut templates: HashMap<(AudioDirection, String), Vec<f32>> = HashMap::new();
    let mut last_snapshot: Option<Arc<AudioSnapshot>> = None;

    let initial = read_alsa_snapshot(
        &mixer,
        &card_name,
        selected_output.as_deref(),
        selected_input.as_deref(),
    );
    record_nonzero_templates(&initial, &mut templates);
    update_snapshot(&shared, &mut last_snapshot, initial);

    while !stop_flag.load(Ordering::Acquire) {
        let mut fds = mixer.get().unwrap_or_default();
        fds.push(pollfd {
            fd: wake_rx.as_raw_fd(),
            events: alsa::poll::Flags::IN.bits(),
            revents: 0,
        });

        if poll(&mut fds, -1).is_err() {
            break;
        }

        // Drain wakeup pipe bytes.
        let mut buf = [0_u8; 64];
        while wake_rx.read(&mut buf).unwrap_or(0) > 0 {}

        let _ = mixer.handle_events();

        let mut shutdown = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                AudioCommand::Shutdown => {
                    shutdown = true;
                    break;
                }
                AudioCommand::SetDefaultDevice(dir, id) => {
                    let devices = list_selems(&mixer, &card_name, dir);
                    if let Some(dev) = devices.into_iter().find(|d| d.id == id) {
                        match dir {
                            AudioDirection::Output => selected_output = Some(dev.name),
                            AudioDirection::Input => selected_input = Some(dev.name),
                        }
                    }
                }
                AudioCommand::SetVolume(target, linear) => {
                    with_target_selem(
                        &mixer,
                        &card_name,
                        target,
                        selected_output.as_deref(),
                        selected_input.as_deref(),
                        &mut templates,
                        |selem, dir, tpl| {
                            if has_volume(selem, dir) {
                                let vol = read_volume(selem, dir);
                                let scaled = scale_linear_channels(
                                    &vol.channels,
                                    tpl,
                                    vol.channels.len().max(1),
                                    linear.clamp(0.0, 1.0),
                                );
                                write_channels(selem, dir, &scaled, tpl);
                            }
                        },
                    );
                }
                AudioCommand::AdjustVolume(target, delta) => {
                    with_target_selem(
                        &mixer,
                        &card_name,
                        target,
                        selected_output.as_deref(),
                        selected_input.as_deref(),
                        &mut templates,
                        |selem, dir, tpl| {
                            if has_volume(selem, dir) {
                                let vol = read_volume(selem, dir);
                                let next_linear = (vol.linear + delta).clamp(0.0, 1.0);
                                let scaled = scale_linear_channels(
                                    &vol.channels,
                                    tpl,
                                    vol.channels.len().max(1),
                                    next_linear,
                                );
                                write_channels(selem, dir, &scaled, tpl);
                            }
                        },
                    );
                }
                AudioCommand::SetBalance(target, balance) => {
                    with_target_selem(
                        &mixer,
                        &card_name,
                        target,
                        selected_output.as_deref(),
                        selected_input.as_deref(),
                        &mut templates,
                        |selem, dir, tpl| {
                            if has_volume(selem, dir) {
                                let vol = read_volume(selem, dir);
                                let balanced = apply_linear_balance_with_map(
                                    &vol.channels,
                                    &vol.channel_map,
                                    vol.channels.len().max(1),
                                    vol.linear,
                                    balance,
                                );
                                write_channels(selem, dir, &balanced, tpl);
                            }
                        },
                    );
                }
                AudioCommand::SetChannelVolumes(target, channels) => {
                    if let Some(clean) = sanitize_linear_channels(&channels) {
                        with_target_selem(
                            &mixer,
                            &card_name,
                            target,
                            selected_output.as_deref(),
                            selected_input.as_deref(),
                            &mut templates,
                            |selem, dir, tpl| {
                                if has_volume(selem, dir) {
                                    write_channels(selem, dir, &clean, tpl);
                                }
                            },
                        );
                    }
                }
                AudioCommand::SetMute(target, muted) => {
                    with_target_selem(
                        &mixer,
                        &card_name,
                        target,
                        selected_output.as_deref(),
                        selected_input.as_deref(),
                        &mut templates,
                        |selem, dir, _| {
                            if has_switch(selem, dir) {
                                set_switch_all(selem, dir, if muted { 0 } else { 1 });
                            }
                        },
                    );
                }
                AudioCommand::ToggleMute(target) => {
                    with_target_selem(
                        &mixer,
                        &card_name,
                        target,
                        selected_output.as_deref(),
                        selected_input.as_deref(),
                        &mut templates,
                        |selem, dir, _| {
                            if has_switch(selem, dir) {
                                let ch = primary_channel(selem, dir);
                                let cur = get_switch(selem, dir, ch).unwrap_or(1);
                                set_switch_all(selem, dir, if cur == 0 { 1 } else { 0 });
                            }
                        },
                    );
                }
                AudioCommand::MoveStream(..)
                | AudioCommand::SetPort(..)
                | AudioCommand::SetCardProfile(..) => {}
            }
        }
        if shutdown {
            break;
        }

        let snap = read_alsa_snapshot(
            &mixer,
            &card_name,
            selected_output.as_deref(),
            selected_input.as_deref(),
        );
        record_nonzero_templates(&snap, &mut templates);
        update_snapshot(&shared, &mut last_snapshot, snap);
    }

    if !stop_flag.load(Ordering::Acquire)
        && let Ok(mut guard) = shared.lock()
    {
        guard.connected = false;
        guard.push_event(AudioEvent::BackendDisconnected);
    }
}

fn record_nonzero_templates(
    snap: &AudioSnapshot,
    templates: &mut HashMap<(AudioDirection, String), Vec<f32>>,
) {
    for dev in &snap.sinks {
        if dev.volume.channels.iter().any(|&v| v > 1e-6) {
            templates.insert(
                (AudioDirection::Output, dev.name.clone()),
                dev.volume.channels.clone(),
            );
        }
    }
    for dev in &snap.sources {
        if dev.volume.channels.iter().any(|&v| v > 1e-6) {
            templates.insert(
                (AudioDirection::Input, dev.name.clone()),
                dev.volume.channels.clone(),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn with_target_selem(
    mixer: &Mixer,
    card_name: &str,
    target: AudioTarget,
    selected_output: Option<&str>,
    selected_input: Option<&str>,
    templates: &mut HashMap<(AudioDirection, String), Vec<f32>>,
    f: impl FnOnce(&Selem<'_>, AudioDirection, &mut Vec<f32>),
) {
    let (dir, explicit_id) = match target {
        AudioTarget::DefaultSink => (AudioDirection::Output, None),
        AudioTarget::Sink(id) => (AudioDirection::Output, Some(id)),
        AudioTarget::DefaultSource => (AudioDirection::Input, None),
        AudioTarget::Source(id) => (AudioDirection::Input, Some(id)),
        AudioTarget::PlaybackStream(_) | AudioTarget::CaptureStream(_) => return,
    };
    let resolved_name = match explicit_id {
        Some(id) => list_selems(mixer, card_name, dir)
            .into_iter()
            .find(|d| d.id == id)
            .map(|d| d.name),
        None => match dir {
            AudioDirection::Output => selected_output.map(str::to_owned),
            AudioDirection::Input => selected_input.map(str::to_owned),
        },
    };
    let Some(name) = resolved_name else {
        return;
    };
    let Some(selem) = mixer.find_selem(&SelemId::new(&name, 0)) else {
        return;
    };
    let tpl = templates.entry((dir, name)).or_default();
    f(&selem, dir, tpl);
}

#[inline]
fn has_volume(selem: &Selem<'_>, dir: AudioDirection) -> bool {
    match dir {
        AudioDirection::Output => selem.has_playback_volume(),
        AudioDirection::Input => selem.has_capture_volume(),
    }
}

#[inline]
fn has_switch(selem: &Selem<'_>, dir: AudioDirection) -> bool {
    match dir {
        AudioDirection::Output => selem.has_playback_switch(),
        AudioDirection::Input => selem.has_capture_switch(),
    }
}

#[inline]
fn is_mono(selem: &Selem<'_>, dir: AudioDirection) -> bool {
    match dir {
        AudioDirection::Output => selem.is_playback_mono(),
        AudioDirection::Input => selem.is_capture_mono(),
    }
}

#[inline]
fn has_channel(selem: &Selem<'_>, dir: AudioDirection, ch: SelemChannelId) -> bool {
    match dir {
        AudioDirection::Output => selem.has_playback_channel(ch),
        AudioDirection::Input => selem.has_capture_channel(ch),
    }
}

#[inline]
fn volume_range(selem: &Selem<'_>, dir: AudioDirection) -> (i64, i64) {
    match dir {
        AudioDirection::Output => selem.get_playback_volume_range(),
        AudioDirection::Input => selem.get_capture_volume_range(),
    }
}

#[inline]
fn get_volume(selem: &Selem<'_>, dir: AudioDirection, ch: SelemChannelId) -> Option<i64> {
    match dir {
        AudioDirection::Output => selem.get_playback_volume(ch).ok(),
        AudioDirection::Input => selem.get_capture_volume(ch).ok(),
    }
}

#[inline]
fn ask_vol_db(selem: &Selem<'_>, dir: AudioDirection, raw: i64) -> Option<f32> {
    match dir {
        AudioDirection::Output => selem.ask_playback_vol_db(raw).ok().map(|mb| mb.to_db()),
        AudioDirection::Input => selem.ask_capture_vol_db(raw).ok().map(|mb| mb.to_db()),
    }
}

#[inline]
fn get_switch(selem: &Selem<'_>, dir: AudioDirection, ch: SelemChannelId) -> Option<i32> {
    match dir {
        AudioDirection::Output => selem.get_playback_switch(ch).ok(),
        AudioDirection::Input => selem.get_capture_switch(ch).ok(),
    }
}

#[inline]
fn set_switch_all(selem: &Selem<'_>, dir: AudioDirection, val: i32) {
    match dir {
        AudioDirection::Output => {
            let _ = selem.set_playback_switch_all(val);
        }
        AudioDirection::Input => {
            let _ = selem.set_capture_switch_all(val);
        }
    }
}

#[inline]
fn set_volume_all(selem: &Selem<'_>, dir: AudioDirection, target: i64) {
    match dir {
        AudioDirection::Output => {
            let _ = selem.set_playback_volume_all(target);
        }
        AudioDirection::Input => {
            let _ = selem.set_capture_volume_all(target);
        }
    }
}

#[inline]
fn set_channel_volume(selem: &Selem<'_>, dir: AudioDirection, ch: SelemChannelId, target: i64) {
    match dir {
        AudioDirection::Output => {
            let _ = selem.set_playback_volume(ch, target);
        }
        AudioDirection::Input => {
            let _ = selem.set_capture_volume(ch, target);
        }
    }
}

fn pick_initial_selem(mixer: &Mixer, dir: AudioDirection) -> Option<String> {
    let preferred = match dir {
        AudioDirection::Output => PREFERRED_OUTPUT_SELEMS,
        AudioDirection::Input => PREFERRED_INPUT_SELEMS,
    };
    for &name in preferred {
        if let Some(selem) = mixer.find_selem(&SelemId::new(name, 0))
            && has_volume(&selem, dir)
        {
            return Some(name.to_owned());
        }
    }
    for elem in mixer.iter() {
        if let Some(selem) = Selem::new(elem)
            && has_volume(&selem, dir)
        {
            let id = selem.get_id();
            if let Ok(name) = id.get_name() {
                return Some(name.to_owned());
            }
        }
    }
    None
}

fn primary_channel(selem: &Selem<'_>, dir: AudioDirection) -> SelemChannelId {
    if has_channel(selem, dir, SelemChannelId::FrontLeft) {
        SelemChannelId::FrontLeft
    } else if has_channel(selem, dir, SelemChannelId::FrontRight) {
        SelemChannelId::FrontRight
    } else {
        SelemChannelId::mono()
    }
}

fn selem_channels(
    selem: &Selem<'_>,
    dir: AudioDirection,
) -> Vec<(SelemChannelId, AudioChannelPosition)> {
    if is_mono(selem, dir) {
        return vec![(primary_channel(selem, dir), AudioChannelPosition::Mono)];
    }
    let mut out = Vec::new();
    for &(ch, pos) in ORDERED_ALSA_CHANNELS {
        if has_channel(selem, dir, ch) {
            out.push((ch, pos));
        }
    }
    if out.is_empty() {
        out.push((primary_channel(selem, dir), AudioChannelPosition::Mono));
    } else if out.len() == 1 {
        out[0].1 = AudioChannelPosition::Mono;
    }
    out
}

fn read_volume(selem: &Selem<'_>, dir: AudioDirection) -> AudioVolume {
    let (min, max) = volume_range(selem, dir);
    let span = (max - min).max(1) as f32;
    let ch_pairs = selem_channels(selem, dir);
    let mut channels = Vec::with_capacity(ch_pairs.len());
    let mut channel_map = Vec::with_capacity(ch_pairs.len());
    let mut peak_raw = min;
    for &(ch, pos) in &ch_pairs {
        let raw = get_volume(selem, dir, ch).unwrap_or(min);
        peak_raw = peak_raw.max(raw);
        channels.push(((raw - min) as f32 / span).clamp(0.0, 1.0));
        channel_map.push(pos);
    }
    let linear = channels.iter().copied().fold(0.0_f32, f32::max);
    let db = ask_vol_db(selem, dir, peak_raw);
    let ch = primary_channel(selem, dir);
    let muted = if has_switch(selem, dir) {
        get_switch(selem, dir, ch).is_some_and(|v| v == 0)
    } else {
        false
    };
    AudioVolume {
        linear,
        channels,
        channel_map,
        db,
        muted,
    }
}

fn list_selems(mixer: &Mixer, card_name: &str, dir: AudioDirection) -> Vec<AudioDevice> {
    let mut out = Vec::new();
    for (idx, elem) in mixer.iter().enumerate() {
        let Some(selem) = Selem::new(elem) else {
            continue;
        };
        if !has_volume(&selem, dir) {
            continue;
        }
        let id = selem.get_id();
        let Ok(name) = id.get_name() else {
            continue;
        };
        out.push(AudioDevice {
            id: idx as u32 + 1,
            name: name.to_owned(),
            description: format!("{name} ({card_name})"),
            is_default: false,
            volume: read_volume(&selem, dir),
            active_port: None,
            ports: Vec::new(),
        });
    }
    out
}

fn finalize_devices(
    mut devices: Vec<AudioDevice>,
    selected_name: Option<&str>,
) -> (Vec<AudioDevice>, Option<u32>) {
    let default_id = selected_name
        .and_then(|n| devices.iter().find(|d| d.name == n).map(|d| d.id))
        .or_else(|| devices.first().map(|d| d.id));
    if let Some(id) = default_id {
        for dev in &mut devices {
            dev.is_default = dev.id == id;
        }
    }
    (devices, default_id)
}

fn read_alsa_snapshot(
    mixer: &Mixer,
    card_name: &str,
    selected_output: Option<&str>,
    selected_input: Option<&str>,
) -> AudioSnapshot {
    let (sinks, default_sink_id) = finalize_devices(
        list_selems(mixer, card_name, AudioDirection::Output),
        selected_output,
    );
    let (sources, default_source_id) = finalize_devices(
        list_selems(mixer, card_name, AudioDirection::Input),
        selected_input,
    );
    AudioSnapshot {
        sinks,
        sources,
        default_sink_id,
        default_source_id,
        streams: Vec::new(),
        cards: Vec::new(),
    }
}

fn write_channels(
    selem: &Selem<'_>,
    dir: AudioDirection,
    values: &[f32],
    nonzero_template: &mut Vec<f32>,
) {
    let (min, max) = volume_range(selem, dir);
    let span = (max - min) as f32;
    let ch_pairs = selem_channels(selem, dir);
    if ch_pairs.len() <= 1 || values.len() <= 1 {
        let clamped = values.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let target = min + (span * clamped).round() as i64;
        set_volume_all(selem, dir, target);
    } else {
        for (idx, &(ch, _)) in ch_pairs.iter().enumerate() {
            let clamped = values
                .get(idx)
                .or_else(|| values.last())
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let target = min + (span * clamped).round() as i64;
            set_channel_volume(selem, dir, ch, target);
        }
    }
    if values.iter().any(|&v| v > 1e-6) {
        nonzero_template.clear();
        nonzero_template.extend_from_slice(values);
    }
}
