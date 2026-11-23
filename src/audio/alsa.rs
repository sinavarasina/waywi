//! ALSA (`alsa` crate) mixer backend implementation.

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
    AudioDevice, AudioEvent, AudioSnapshot, apply_linear_balance, sanitize_linear_channels,
    scale_linear_channels,
};

const PREFERRED_SELEM_NAMES: &[&str] = &["Master", "PCM", "Speaker", "Headphone", "DAC"];
const PREFERRED_CAPTURE_SELEM_NAMES: &[&str] = &[
    "Capture",
    "Mic",
    "Internal Mic",
    "Front Mic",
    "Headset Mic",
    "Input",
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
        guard.push_event(AudioEvent::Reconnected);
    }

    let mut selected_selem: Option<String> = pick_initial_selem(&mixer);
    let mut selected_capture_selem: Option<String> = pick_initial_capture_selem(&mixer);
    let mut playback_template: Vec<f32> = Vec::new();
    let mut capture_template: Vec<f32> = Vec::new();
    let mut last_snapshot: Option<AudioSnapshot> = None;
    let initial = read_alsa_snapshot(
        &mixer,
        &card_name,
        selected_selem.as_deref(),
        selected_capture_selem.as_deref(),
    );
    if initial.channels.iter().any(|&v| v > 1e-6) {
        playback_template.clone_from(&initial.channels);
    }
    if initial.source_channels.iter().any(|&v| v > 1e-6) {
        capture_template.clone_from(&initial.source_channels);
    }
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
                AudioCommand::SelectDevice(id) => {
                    let devices = list_playback_selems(&mixer, &card_name);
                    if let Some(dev) = devices.into_iter().find(|d| d.id == id) {
                        selected_selem = Some(dev.name);
                        playback_template.clear();
                    }
                }
                AudioCommand::SelectSource(id) => {
                    let devices = list_capture_selems(&mixer, &card_name);
                    if let Some(dev) = devices.into_iter().find(|d| d.id == id) {
                        selected_capture_selem = Some(dev.name);
                        capture_template.clear();
                    }
                }
                cmd @ (AudioCommand::SetSourceVolumeLinear(_)
                | AudioCommand::SetSourceBalance(_)
                | AudioCommand::SetSourceChannelVolumes(_)
                | AudioCommand::SetSourceMuted(_)
                | AudioCommand::ToggleSourceMute) => {
                    if let Some(name) = &selected_capture_selem
                        && let Some(selem) = mixer.find_selem(&SelemId::new(name, 0))
                    {
                        apply_alsa_source_command(&selem, cmd, &mut capture_template);
                    }
                }
                other => {
                    if let Some(name) = &selected_selem
                        && let Some(selem) = mixer.find_selem(&SelemId::new(name, 0))
                    {
                        apply_alsa_command(&selem, other, &mut playback_template);
                    }
                }
            }
        }
        if shutdown {
            break;
        }

        let snap = read_alsa_snapshot(
            &mixer,
            &card_name,
            selected_selem.as_deref(),
            selected_capture_selem.as_deref(),
        );
        if snap.channels.iter().any(|&v| v > 1e-6) {
            playback_template.clone_from(&snap.channels);
        }
        if snap.source_channels.iter().any(|&v| v > 1e-6) {
            capture_template.clone_from(&snap.source_channels);
        }
        update_snapshot(&shared, &mut last_snapshot, snap);
    }

    if let Ok(mut guard) = shared.lock() {
        guard.connected = false;
        guard.push_event(AudioEvent::Disconnected);
    }
}

fn pick_initial_selem(mixer: &Mixer) -> Option<String> {
    for &preferred in PREFERRED_SELEM_NAMES {
        if let Some(selem) = mixer.find_selem(&SelemId::new(preferred, 0))
            && selem.can_playback()
        {
            return Some(preferred.to_owned());
        }
    }
    for elem in mixer.iter() {
        if let Some(selem) = Selem::new(elem)
            && selem.has_playback_volume()
        {
            let id = selem.get_id();
            if let Ok(name) = id.get_name() {
                return Some(name.to_owned());
            }
        }
    }
    None
}

fn pick_initial_capture_selem(mixer: &Mixer) -> Option<String> {
    for &preferred in PREFERRED_CAPTURE_SELEM_NAMES {
        if let Some(selem) = mixer.find_selem(&SelemId::new(preferred, 0))
            && selem.has_capture_volume()
        {
            return Some(preferred.to_owned());
        }
    }
    for elem in mixer.iter() {
        if let Some(selem) = Selem::new(elem)
            && selem.has_capture_volume()
        {
            let id = selem.get_id();
            if let Ok(name) = id.get_name() {
                return Some(name.to_owned());
            }
        }
    }
    None
}

fn list_playback_selems(mixer: &Mixer, card_name: &str) -> Vec<AudioDevice> {
    let mut out = Vec::new();
    for (idx, elem) in mixer.iter().enumerate() {
        let Some(selem) = Selem::new(elem) else {
            continue;
        };
        if !selem.has_playback_volume() {
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
            ..AudioDevice::default()
        });
    }
    out
}

fn list_capture_selems(mixer: &Mixer, card_name: &str) -> Vec<AudioDevice> {
    let mut out = Vec::new();
    for (idx, elem) in mixer.iter().enumerate() {
        let Some(selem) = Selem::new(elem) else {
            continue;
        };
        if !selem.has_capture_volume() {
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
            ..AudioDevice::default()
        });
    }
    out
}

fn primary_playback_channel(selem: &Selem<'_>) -> SelemChannelId {
    if selem.has_playback_channel(SelemChannelId::FrontLeft) {
        SelemChannelId::FrontLeft
    } else if selem.has_playback_channel(SelemChannelId::FrontRight) {
        SelemChannelId::FrontRight
    } else {
        SelemChannelId::mono()
    }
}

fn primary_capture_channel(selem: &Selem<'_>) -> SelemChannelId {
    if selem.has_capture_channel(SelemChannelId::FrontLeft) {
        SelemChannelId::FrontLeft
    } else if selem.has_capture_channel(SelemChannelId::FrontRight) {
        SelemChannelId::FrontRight
    } else {
        SelemChannelId::mono()
    }
}

fn playback_channels(selem: &Selem<'_>) -> Vec<SelemChannelId> {
    if selem.has_playback_channel(SelemChannelId::FrontLeft)
        && selem.has_playback_channel(SelemChannelId::FrontRight)
    {
        vec![SelemChannelId::FrontLeft, SelemChannelId::FrontRight]
    } else {
        vec![primary_playback_channel(selem)]
    }
}

fn capture_channels(selem: &Selem<'_>) -> Vec<SelemChannelId> {
    if selem.has_capture_channel(SelemChannelId::FrontLeft)
        && selem.has_capture_channel(SelemChannelId::FrontRight)
    {
        vec![SelemChannelId::FrontLeft, SelemChannelId::FrontRight]
    } else {
        vec![primary_capture_channel(selem)]
    }
}

fn read_alsa_snapshot(
    mixer: &Mixer,
    card_name: &str,
    selected_name: Option<&str>,
    selected_source_name: Option<&str>,
) -> AudioSnapshot {
    let devices = list_playback_selems(mixer, card_name);
    let sources = list_capture_selems(mixer, card_name);
    let default_sink = selected_name
        .and_then(|n| devices.iter().find(|d| d.name == n).cloned())
        .or_else(|| devices.first().cloned());
    let default_source = selected_source_name
        .and_then(|n| sources.iter().find(|d| d.name == n).cloned())
        .or_else(|| sources.first().cloned());

    let (linear, channels, db, muted) = if let Some(ref dev) = default_sink
        && let Some(selem) = mixer.find_selem(&SelemId::new(&dev.name, 0))
    {
        read_playback_levels(&selem)
    } else {
        (0.0, Vec::new(), None, false)
    };

    let (source_linear, source_channels, source_db, source_muted) = if let Some(ref dev) =
        default_source
        && let Some(selem) = mixer.find_selem(&SelemId::new(&dev.name, 0))
    {
        read_capture_levels(&selem)
    } else {
        (0.0, Vec::new(), None, false)
    };

    AudioSnapshot {
        linear,
        channels,
        db,
        muted,
        default_sink,
        devices,
        source_linear,
        source_channels,
        source_db,
        source_muted,
        default_source,
        sources,
        ..AudioSnapshot::default()
    }
}

fn read_playback_levels(selem: &Selem<'_>) -> (f32, Vec<f32>, Option<f32>, bool) {
    let (min, max) = selem.get_playback_volume_range();
    let span = (max - min).max(1) as f32;
    let ch_ids = playback_channels(selem);
    let mut channels = Vec::with_capacity(ch_ids.len());
    let mut peak_raw = min;
    for &ch in &ch_ids {
        let raw = selem.get_playback_volume(ch).unwrap_or(min);
        peak_raw = peak_raw.max(raw);
        channels.push(((raw - min) as f32 / span).clamp(0.0, 1.0));
    }
    let linear = channels.iter().copied().fold(0.0_f32, f32::max);
    let db = selem
        .ask_playback_vol_db(peak_raw)
        .ok()
        .map(|mb| mb.to_db());
    let ch = primary_playback_channel(selem);
    let muted = if selem.has_playback_switch() {
        selem
            .get_playback_switch(ch)
            .map(|v| v == 0)
            .unwrap_or(false)
    } else {
        false
    };
    (linear, channels, db, muted)
}

fn read_capture_levels(selem: &Selem<'_>) -> (f32, Vec<f32>, Option<f32>, bool) {
    let (min, max) = selem.get_capture_volume_range();
    let span = (max - min).max(1) as f32;
    let ch_ids = capture_channels(selem);
    let mut channels = Vec::with_capacity(ch_ids.len());
    let mut peak_raw = min;
    for &ch in &ch_ids {
        let raw = selem.get_capture_volume(ch).unwrap_or(min);
        peak_raw = peak_raw.max(raw);
        channels.push(((raw - min) as f32 / span).clamp(0.0, 1.0));
    }
    let linear = channels.iter().copied().fold(0.0_f32, f32::max);
    let db = selem.ask_capture_vol_db(peak_raw).ok().map(|mb| mb.to_db());
    let ch = primary_capture_channel(selem);
    let muted = if selem.has_capture_switch() {
        selem
            .get_capture_switch(ch)
            .map(|v| v == 0)
            .unwrap_or(false)
    } else {
        false
    };
    (linear, channels, db, muted)
}

fn write_playback_channels(selem: &Selem<'_>, values: &[f32], nonzero_template: &mut Vec<f32>) {
    let (min, max) = selem.get_playback_volume_range();
    let span = (max - min) as f32;
    let ch_ids = playback_channels(selem);
    if ch_ids.len() <= 1 || values.len() <= 1 {
        let clamped = values.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let target = min + (span * clamped).round() as i64;
        let _ = selem.set_playback_volume_all(target);
    } else {
        for (idx, &ch) in ch_ids.iter().enumerate() {
            let clamped = values
                .get(idx)
                .or_else(|| values.last())
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let target = min + (span * clamped).round() as i64;
            let _ = selem.set_playback_volume(ch, target);
        }
    }
    if values.iter().any(|&v| v > 1e-6) {
        nonzero_template.clear();
        nonzero_template.extend_from_slice(values);
    }
}

fn write_capture_channels(selem: &Selem<'_>, values: &[f32], nonzero_template: &mut Vec<f32>) {
    let (min, max) = selem.get_capture_volume_range();
    let span = (max - min) as f32;
    let ch_ids = capture_channels(selem);
    if ch_ids.len() <= 1 || values.len() <= 1 {
        let clamped = values.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let target = min + (span * clamped).round() as i64;
        let _ = selem.set_capture_volume_all(target);
    } else {
        for (idx, &ch) in ch_ids.iter().enumerate() {
            let clamped = values
                .get(idx)
                .or_else(|| values.last())
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let target = min + (span * clamped).round() as i64;
            let _ = selem.set_capture_volume(ch, target);
        }
    }
    if values.iter().any(|&v| v > 1e-6) {
        nonzero_template.clear();
        nonzero_template.extend_from_slice(values);
    }
}

fn apply_alsa_command(selem: &Selem<'_>, cmd: AudioCommand, nonzero_template: &mut Vec<f32>) {
    match cmd {
        AudioCommand::SetVolumeLinear(linear) => {
            if selem.has_playback_volume() {
                let (_, cur_channels, _, _) = read_playback_levels(selem);
                let scaled = scale_linear_channels(
                    &cur_channels,
                    nonzero_template,
                    cur_channels.len().max(1),
                    linear.clamp(0.0, 1.0),
                );
                write_playback_channels(selem, &scaled, nonzero_template);
            }
        }
        AudioCommand::SetBalance(balance) => {
            if selem.has_playback_volume() {
                let (peak, cur_channels, _, _) = read_playback_levels(selem);
                let balanced =
                    apply_linear_balance(&cur_channels, cur_channels.len().max(1), peak, balance);
                write_playback_channels(selem, &balanced, nonzero_template);
            }
        }
        AudioCommand::SetChannelVolumes(channels) => {
            if selem.has_playback_volume()
                && let Some(channels) = sanitize_linear_channels(&channels)
            {
                write_playback_channels(selem, &channels, nonzero_template);
            }
        }
        AudioCommand::SetMuted(muted) => {
            if selem.has_playback_switch() {
                let _ = selem.set_playback_switch_all(if muted { 0 } else { 1 });
            }
        }
        AudioCommand::ToggleMute => {
            if selem.has_playback_switch() {
                let ch = primary_playback_channel(selem);
                let cur = selem.get_playback_switch(ch).unwrap_or(1);
                let _ = selem.set_playback_switch_all(if cur == 0 { 1 } else { 0 });
            }
        }
        AudioCommand::SelectDevice(_)
        | AudioCommand::SelectPort(..)
        | AudioCommand::SetSourceVolumeLinear(_)
        | AudioCommand::SetSourceBalance(_)
        | AudioCommand::SetSourceChannelVolumes(_)
        | AudioCommand::SetSourceMuted(_)
        | AudioCommand::ToggleSourceMute
        | AudioCommand::SelectSource(_)
        | AudioCommand::SelectSourcePort(..)
        | AudioCommand::SelectCardProfile(..)
        | AudioCommand::SetStreamVolumeLinear(..)
        | AudioCommand::SetStreamBalance(..)
        | AudioCommand::SetStreamChannelVolumes(..)
        | AudioCommand::SetStreamMuted(..)
        | AudioCommand::ToggleStreamMute(_)
        | AudioCommand::SetCaptureVolumeLinear(..)
        | AudioCommand::SetCaptureBalance(..)
        | AudioCommand::SetCaptureChannelVolumes(..)
        | AudioCommand::SetCaptureMuted(..)
        | AudioCommand::ToggleCaptureMute(_)
        | AudioCommand::Shutdown => {}
    }
}

fn apply_alsa_source_command(
    selem: &Selem<'_>,
    cmd: AudioCommand,
    nonzero_template: &mut Vec<f32>,
) {
    match cmd {
        AudioCommand::SetSourceVolumeLinear(linear) => {
            if selem.has_capture_volume() {
                let (_, cur_channels, _, _) = read_capture_levels(selem);
                let scaled = scale_linear_channels(
                    &cur_channels,
                    nonzero_template,
                    cur_channels.len().max(1),
                    linear.clamp(0.0, 1.0),
                );
                write_capture_channels(selem, &scaled, nonzero_template);
            }
        }
        AudioCommand::SetSourceBalance(balance) => {
            if selem.has_capture_volume() {
                let (peak, cur_channels, _, _) = read_capture_levels(selem);
                let balanced =
                    apply_linear_balance(&cur_channels, cur_channels.len().max(1), peak, balance);
                write_capture_channels(selem, &balanced, nonzero_template);
            }
        }
        AudioCommand::SetSourceChannelVolumes(channels) => {
            if selem.has_capture_volume()
                && let Some(channels) = sanitize_linear_channels(&channels)
            {
                write_capture_channels(selem, &channels, nonzero_template);
            }
        }
        AudioCommand::SetSourceMuted(muted) => {
            if selem.has_capture_switch() {
                let _ = selem.set_capture_switch_all(if muted { 0 } else { 1 });
            }
        }
        AudioCommand::ToggleSourceMute => {
            if selem.has_capture_switch() {
                let ch = primary_capture_channel(selem);
                let cur = selem.get_capture_switch(ch).unwrap_or(1);
                let _ = selem.set_capture_switch_all(if cur == 0 { 1 } else { 0 });
            }
        }
        other => {
            let _ = other;
        }
    }
}
