//! Backend-agnostic internal contract for `waywi` audio providers.
//!
//! Backends (`pipewire`, `pulseaudio`, and `alsa`, with room reserved for JACK) implement
//! `AudioBackend` and push normalized snapshots/events into `SharedAudioState`.

#[cfg(any(feature = "pulseaudio", feature = "alsa"))]
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::error::Result;

use super::types::{AudioEvent, AudioSnapshot};

/// Commands sent from [`super::AudioClient`] to the active audio backend worker.
#[derive(Debug, Clone)]
pub(crate) enum AudioCommand {
    SetVolumeLinear(f32),
    SetMuted(bool),
    ToggleMute,
    SelectDevice(u32),
    SetSourceVolumeLinear(f32),
    SetSourceMuted(bool),
    ToggleSourceMute,
    SelectSource(u32),
    Shutdown,
}

/// Thread-safe state shared between [`super::AudioClient`] and the active backend worker.
#[derive(Default)]
pub(crate) struct SharedAudioState {
    pub(crate) snapshot: Option<AudioSnapshot>,
    pub(crate) pending_events: VecDeque<AudioEvent>,
    pub(crate) wakeup: Option<Box<dyn Fn() + Send + Sync>>,
    pub(crate) connected: bool,
}

impl SharedAudioState {
    /// Enqueues an event (bounded to 256 pending events) and triggers any registered wakeup hook.
    pub(crate) fn push_event(&mut self, event: AudioEvent) {
        if self.pending_events.len() >= 256 {
            self.pending_events.pop_front();
        }
        self.pending_events.push_back(event);
        if let Some(wake) = &self.wakeup {
            wake();
        }
    }
}

/// Command channel handle exposed by a running audio backend worker.
pub(crate) trait AudioCommandSender: Send {
    /// Delivers a control command to the backend worker loop.
    fn send(&self, cmd: AudioCommand) -> Result<()>;
}

/// Abstract audio backend capable of spawning a background worker thread.
pub(crate) trait AudioBackend: Send + Sync {
    /// Spawns the backend worker and returns its command sender plus thread join handle.
    fn start(
        &self,
        shared: Arc<Mutex<SharedAudioState>>,
        stop_flag: Arc<AtomicBool>,
    ) -> Option<(Box<dyn AudioCommandSender>, JoinHandle<()>)>;
}

/// Compares `next` against `last_snapshot`, emitting `DeviceAdded`/`DeviceRemoved`/`Updated`
/// events into `shared` cleanly across audio backends.
#[cfg(any(feature = "pulseaudio", feature = "alsa"))]
pub(crate) fn update_snapshot(
    shared: &Arc<Mutex<SharedAudioState>>,
    last_snapshot: &mut Option<AudioSnapshot>,
    next: AudioSnapshot,
) {
    if last_snapshot.as_ref() == Some(&next) {
        return;
    }

    let mut events = Vec::new();
    if let Some(prev) = last_snapshot.as_ref() {
        let prev_ids: HashSet<u32> = prev.devices.iter().map(|d| d.id).collect();
        let next_ids: HashSet<u32> = next.devices.iter().map(|d| d.id).collect();
        for dev in &prev.devices {
            if !next_ids.contains(&dev.id) {
                events.push(AudioEvent::DeviceRemoved(dev.id));
            }
        }
        for dev in &next.devices {
            if !prev_ids.contains(&dev.id) {
                events.push(AudioEvent::DeviceAdded(dev.clone()));
            }
        }
    } else {
        for dev in &next.devices {
            events.push(AudioEvent::DeviceAdded(dev.clone()));
        }
    }

    *last_snapshot = Some(next.clone());
    if let Ok(mut guard) = shared.lock() {
        guard.snapshot = Some(next.clone());
        for ev in events {
            guard.push_event(ev);
        }
        guard.push_event(AudioEvent::Updated(next));
    }
}

#[cfg(feature = "pipewire")]
pub(crate) fn pipewire_runtime_available() -> bool {
    if std::env::var_os("PIPEWIRE_REMOTE").is_some() {
        return true;
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .is_some_and(|dir| dir.join("pipewire-0").exists())
}

#[cfg(feature = "pulseaudio")]
pub(crate) fn pulseaudio_runtime_available() -> bool {
    if std::env::var_os("PULSE_SERVER").is_some() {
        return true;
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .is_some_and(|dir| dir.join("pulse").join("native").exists())
}

#[cfg(feature = "alsa")]
pub(crate) fn alsa_runtime_available() -> bool {
    std::path::Path::new("/dev/snd").exists()
}

/// Selects the highest-priority audio backend enabled by Cargo features and live session sockets.
#[must_use]
pub(crate) fn default_backend() -> Box<dyn AudioBackend> {
    #[cfg(feature = "pipewire")]
    if pipewire_runtime_available() {
        return Box::new(super::pipewire::PipewireBackend);
    }

    #[cfg(feature = "pulseaudio")]
    if pulseaudio_runtime_available() {
        return Box::new(super::pulse::PulseBackend);
    }

    #[cfg(feature = "alsa")]
    if alsa_runtime_available() {
        return Box::new(super::alsa::AlsaBackend);
    }

    #[cfg(feature = "pipewire")]
    {
        Box::new(super::pipewire::PipewireBackend)
    }
    #[cfg(all(not(feature = "pipewire"), feature = "pulseaudio"))]
    {
        Box::new(super::pulse::PulseBackend)
    }
    #[cfg(all(
        not(feature = "pipewire"),
        not(feature = "pulseaudio"),
        feature = "alsa"
    ))]
    {
        Box::new(super::alsa::AlsaBackend)
    }
}
