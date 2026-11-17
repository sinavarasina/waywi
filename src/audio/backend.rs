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
///
/// Per-app stream commands carry the stream ID directly (sink input / source output index on
/// PulseAudio, node ID on PipeWire); unknown IDs are no-ops so stale UI commands stay harmless.
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
    /// Per-app stream commands are only observed by the PulseAudio and PipeWire backends; the
    /// ALSA mixer has no per-application routing.
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    SetStreamVolumeLinear(u32, f32),
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    SetStreamMuted(u32, bool),
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    ToggleStreamMute(u32),
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    SetCaptureVolumeLinear(u32, f32),
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    SetCaptureMuted(u32, bool),
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    ToggleCaptureMute(u32),
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

/// Resolves the human-readable name of a per-app stream: PulseAudio/PipeWire `application.name`,
/// falling back to `application.process.binary`, then the raw stream name, then `"unknown"`; blank
/// candidates are skipped at each step.
#[cfg_attr(
    not(any(feature = "pulseaudio", feature = "pipewire")),
    allow(dead_code)
)]
#[must_use]
pub(crate) fn stream_display_name(
    app_name: Option<&str>,
    binary: Option<&str>,
    stream_name: Option<&str>,
) -> String {
    let candidate = app_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| binary.map(str::trim).filter(|s| !s.is_empty()))
        .or_else(|| stream_name.map(str::trim).filter(|s| !s.is_empty()));
    candidate.unwrap_or("unknown").to_owned()
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

#[cfg(test)]
mod tests {
    use super::{AudioCommand, stream_display_name};

    #[test]
    fn stream_display_name_prefers_app_then_binary_then_stream_name() {
        assert_eq!(
            stream_display_name(Some("Firefox"), Some("firefox"), Some("Firefox input")),
            "Firefox"
        );
        assert_eq!(
            stream_display_name(None, Some("spotify"), Some("Spotify")),
            "spotify"
        );
        assert_eq!(
            stream_display_name(None, None, Some("PulseAudio volume control")),
            "PulseAudio volume control"
        );
        assert_eq!(stream_display_name(None, None, None), "unknown");
        assert_eq!(stream_display_name(Some("  "), None, Some("node")), "node");
    }

    #[test]
    fn stream_commands_clone_with_values() {
        let cmds = [
            AudioCommand::SetStreamVolumeLinear(7, 0.5),
            AudioCommand::SetStreamMuted(7, true),
            AudioCommand::ToggleStreamMute(7),
            AudioCommand::SetCaptureVolumeLinear(9, 1.5),
            AudioCommand::SetCaptureMuted(9, true),
            AudioCommand::ToggleCaptureMute(9),
        ];
        let cloned = cmds.clone();
        assert!(matches!(
            cloned[0],
            AudioCommand::SetStreamVolumeLinear(id, v) if id == 7 && (v - 0.5).abs() < 1e-6
        ));
        assert!(matches!(
            cloned[1],
            AudioCommand::SetStreamMuted(id, m) if id == 7 && m
        ));
        assert!(matches!(cloned[2], AudioCommand::ToggleStreamMute(7)));
        assert!(matches!(
            cloned[3],
            AudioCommand::SetCaptureVolumeLinear(id, v) if id == 9 && (v - 1.5).abs() < 1e-6
        ));
        assert!(matches!(
            cloned[4],
            AudioCommand::SetCaptureMuted(id, m) if id == 9 && m
        ));
        assert!(matches!(cloned[5], AudioCommand::ToggleCaptureMute(9)));
    }
}

#[cfg(any(feature = "pulseaudio", feature = "alsa"))]
#[cfg(test)]
mod snapshot_tests {
    use std::sync::{Arc, Mutex};

    use super::{SharedAudioState, update_snapshot};
    use crate::audio::types::{AudioDevice, AudioEvent, AudioSnapshot, AudioStream};

    fn snapshot_with_stream(id: u32, linear: f32) -> AudioSnapshot {
        AudioSnapshot {
            devices: vec![AudioDevice {
                id: 1,
                name: "alsa_output.pci".into(),
                description: "Built-in".into(),
            }],
            default_sink: None,
            streams: vec![AudioStream {
                id,
                name: "Firefox".into(),
                binary: Some("firefox".into()),
                sink_id: Some(1),
                linear,
                db: None,
                muted: false,
            }],
            ..AudioSnapshot::default()
        }
    }

    #[test]
    fn update_snapshot_emits_updated_on_stream_volume_change_only() {
        let shared = Arc::new(Mutex::new(SharedAudioState::default()));
        let mut last = None;

        let first = snapshot_with_stream(3, 1.0);
        update_snapshot(&shared, &mut last, first);
        let guard = shared.lock().unwrap();
        assert!(matches!(
            guard.pending_events[0],
            AudioEvent::DeviceAdded(_)
        ));
        assert!(matches!(
            guard.pending_events.back(),
            Some(AudioEvent::Updated(_))
        ));
        assert_eq!(guard.pending_events.len(), 2);
        drop(guard);

        let second = snapshot_with_stream(3, 0.5);
        update_snapshot(&shared, &mut last, second);
        let guard = shared.lock().unwrap();
        assert_eq!(guard.pending_events.len(), 3);
        match guard.pending_events.back() {
            Some(AudioEvent::Updated(snap)) => assert!((snap.streams[0].linear - 0.5).abs() < 1e-6),
            other => panic!("expected Updated event, got {:?}", other),
        }
    }

    #[test]
    fn update_snapshot_skips_identical_stream_snapshots() {
        let shared = Arc::new(Mutex::new(SharedAudioState::default()));
        let mut last = None;

        update_snapshot(&shared, &mut last, snapshot_with_stream(3, 1.0));
        update_snapshot(&shared, &mut last, snapshot_with_stream(3, 1.0));
        let guard = shared.lock().unwrap();
        assert_eq!(guard.pending_events.len(), 2);
    }
}
