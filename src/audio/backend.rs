//! Backend-agnostic internal contract for `waywi` audio providers.
//!
//! Backends (`pipewire`, `pulseaudio`, and `alsa`, with room reserved for JACK) implement
//! `AudioBackend` and push normalized snapshots/events into `SharedAudioState`.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::error::Result;

use super::types::{
    AudioDevice, AudioDirection, AudioEvent, AudioPortTarget, AudioSnapshot, AudioTarget,
};

/// Orthogonal control commands sent from [`super::AudioClient`] to the active audio backend worker.
///
/// Unknown device/stream IDs are treated as no-ops so stale UI commands stay harmless.
#[derive(Debug, Clone)]
pub(crate) enum AudioCommand {
    SetVolume(AudioTarget, f32),
    AdjustVolume(AudioTarget, f32),
    SetBalance(AudioTarget, f32),
    SetChannelVolumes(AudioTarget, Vec<f32>),
    SetMute(AudioTarget, bool),
    ToggleMute(AudioTarget),
    SetDefaultDevice(AudioDirection, u32),
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    MoveStream(AudioDirection, u32, u32),
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    SetPort(AudioPortTarget, String),
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    SetCardProfile(u32, String),
    Shutdown,
}

/// Thread-safe state shared between [`super::AudioClient`] and the active backend worker.
pub(crate) struct SharedAudioState {
    pub(crate) snapshot: Option<Arc<AudioSnapshot>>,
    pub(crate) pending_snapshot: Option<Arc<AudioSnapshot>>,
    pub(crate) pending_events: VecDeque<AudioEvent>,
    pub(crate) has_pending: Arc<AtomicBool>,
    pub(crate) wakeup: Option<Box<dyn Fn() + Send + Sync>>,
    pub(crate) connected: bool,
}

impl Default for SharedAudioState {
    fn default() -> Self {
        Self::new(Arc::new(AtomicBool::new(false)))
    }
}

impl SharedAudioState {
    pub(crate) fn new(has_pending: Arc<AtomicBool>) -> Self {
        Self {
            snapshot: None,
            pending_snapshot: None,
            pending_events: VecDeque::new(),
            has_pending,
            wakeup: None,
            connected: false,
        }
    }

    pub(crate) fn wake(&self) {
        if let Some(wake) = &self.wakeup {
            wake();
        }
    }

    /// Enqueues an event (bounded to 256 pending events) and triggers any registered wakeup hook.
    pub(crate) fn push_event(&mut self, event: AudioEvent) {
        if self.pending_events.len() >= 256 {
            self.pending_events.pop_front();
        }
        self.pending_events.push_back(event);
        self.has_pending.store(true, Ordering::Release);
        self.wake();
    }

    /// Stores a new `Arc<AudioSnapshot>`, appends associated diff events, marks pending, and wakes
    /// the event loop once.
    pub(crate) fn publish_snapshot(
        &mut self,
        snapshot: Arc<AudioSnapshot>,
        events: Vec<AudioEvent>,
    ) {
        self.snapshot = Some(Arc::clone(&snapshot));
        self.pending_snapshot = Some(snapshot);
        for ev in events {
            if self.pending_events.len() >= 256 {
                self.pending_events.pop_front();
            }
            self.pending_events.push_back(ev);
        }
        self.has_pending.store(true, Ordering::Release);
        self.wake();
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

fn diff_devices(
    direction: AudioDirection,
    prev_devices: Option<&[AudioDevice]>,
    next_devices: &[AudioDevice],
    events: &mut Vec<AudioEvent>,
) {
    if let Some(prev_list) = prev_devices {
        let prev_ids: HashSet<u32> = prev_list.iter().map(|d| d.id).collect();
        let next_ids: HashSet<u32> = next_devices.iter().map(|d| d.id).collect();
        for dev in prev_list {
            if !next_ids.contains(&dev.id) {
                events.push(AudioEvent::DeviceRemoved {
                    direction,
                    id: dev.id,
                });
            }
        }
        for dev in next_devices {
            if !prev_ids.contains(&dev.id) {
                events.push(AudioEvent::DeviceAdded {
                    direction,
                    id: dev.id,
                    name: dev.name.clone(),
                });
            } else if let Some(prev_dev) = prev_list.iter().find(|d| d.id == dev.id)
                && prev_dev.active_port != dev.active_port
            {
                events.push(AudioEvent::PortChanged {
                    direction,
                    device_id: dev.id,
                    active_port: dev.active_port.clone(),
                });
            }
        }
    } else {
        for dev in next_devices {
            events.push(AudioEvent::DeviceAdded {
                direction,
                id: dev.id,
                name: dev.name.clone(),
            });
        }
    }
}

/// Compares `next` against `last_snapshot`, emitting granular [`AudioEvent`] transitions for sinks,
/// sources, defaults, volumes, ports, and card profiles, and publishing `Arc<AudioSnapshot>` into
/// `shared` cleanly across all audio backends.
pub(crate) fn update_snapshot(
    shared: &Arc<Mutex<SharedAudioState>>,
    last_snapshot: &mut Option<Arc<AudioSnapshot>>,
    next: AudioSnapshot,
) {
    if last_snapshot.as_deref() == Some(&next) {
        return;
    }

    let mut events = Vec::new();
    let prev = last_snapshot.as_deref();

    diff_devices(
        AudioDirection::Output,
        prev.map(|p| p.sinks.as_slice()),
        &next.sinks,
        &mut events,
    );
    diff_devices(
        AudioDirection::Input,
        prev.map(|p| p.sources.as_slice()),
        &next.sources,
        &mut events,
    );

    if let Some(prev) = prev {
        let prev_sink_name = prev.default_sink().map(|d| d.name.as_str());
        let next_sink_name = next.default_sink().map(|d| d.name.as_str());
        if prev_sink_name != next_sink_name {
            events.push(AudioEvent::DefaultDeviceChanged {
                direction: AudioDirection::Output,
                name: next_sink_name.map(str::to_owned),
            });
        }

        let prev_source_name = prev.default_source().map(|d| d.name.as_str());
        let next_source_name = next.default_source().map(|d| d.name.as_str());
        if prev_source_name != next_source_name {
            events.push(AudioEvent::DefaultDeviceChanged {
                direction: AudioDirection::Input,
                name: next_source_name.map(str::to_owned),
            });
        }

        if let Some(next_sink) = next.default_sink()
            && prev.default_sink().map(|d| &d.volume) != Some(&next_sink.volume)
        {
            events.push(AudioEvent::VolumeChanged {
                direction: AudioDirection::Output,
                volume: next_sink.volume.clone(),
            });
        }

        if let Some(next_source) = next.default_source()
            && prev.default_source().map(|d| &d.volume) != Some(&next_source.volume)
        {
            events.push(AudioEvent::VolumeChanged {
                direction: AudioDirection::Input,
                volume: next_source.volume.clone(),
            });
        }

        for card in &next.cards {
            if let Some(prev_card) = prev.cards.iter().find(|c| c.id == card.id)
                && prev_card.active_profile != card.active_profile
            {
                events.push(AudioEvent::CardProfileChanged {
                    card_id: card.id,
                    active_profile: card.active_profile.clone(),
                });
            }
        }
    }

    let next_arc = Arc::new(next);
    *last_snapshot = Some(Arc::clone(&next_arc));
    if let Ok(mut guard) = shared.lock() {
        guard.publish_snapshot(next_arc, events);
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

/// Normalized per-application stream metadata extracted identically across audio backends.
#[cfg_attr(
    not(any(feature = "pulseaudio", feature = "pipewire")),
    allow(dead_code)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamMetadata {
    pub(crate) name: String,
    pub(crate) app_name: Option<String>,
    pub(crate) binary_name: Option<String>,
    pub(crate) media_role: Option<String>,
}

impl StreamMetadata {
    /// Lazily resolves display name, application name, binary name, and media role from a property
    /// lookup function.
    #[cfg_attr(
        not(any(feature = "pulseaudio", feature = "pipewire")),
        allow(dead_code)
    )]
    pub(crate) fn extract<S: AsRef<str>>(
        stream_name: Option<&str>,
        mut get_prop: impl FnMut(&str) -> Option<S>,
    ) -> Self {
        let app_name = first_non_empty_prop_by(&["application.name"], &mut get_prop);
        let binary_name = first_non_empty_prop_by(&["application.process.binary"], &mut get_prop);
        let media_role = first_non_empty_prop_by(&["media.role"], &mut get_prop);
        let name = stream_display_name(app_name.as_deref(), binary_name.as_deref(), stream_name);
        Self {
            name,
            app_name,
            binary_name,
            media_role,
        }
    }
}

/// Returns the first trimmed, non-empty property string yielded by lazily querying `keys`.
#[cfg_attr(
    not(any(feature = "pulseaudio", feature = "pipewire")),
    allow(dead_code)
)]
#[must_use]
pub(crate) fn first_non_empty_prop_by<S: AsRef<str>>(
    keys: &[&str],
    mut get_prop: impl FnMut(&str) -> Option<S>,
) -> Option<String> {
    keys.iter().find_map(|&key| {
        let val = get_prop(key)?;
        let trimmed = val.as_ref().trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

/// Returns the first trimmed, non-empty property string from `candidates`.
#[must_use]
pub(crate) fn first_non_empty_prop<'a>(
    candidates: impl IntoIterator<Item = Option<&'a str>>,
) -> Option<String> {
    candidates
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_owned)
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
    first_non_empty_prop([app_name, binary, stream_name]).unwrap_or_else(|| "unknown".to_owned())
}

/// Returns compiled-in audio backends ordered by runtime session availability (`PipeWire ->
/// PulseAudio -> ALSA`), followed by any remaining compiled-in fallback backends so enabling
/// multiple audio features never hides a fallback at compile time.
#[must_use]
pub(crate) fn candidate_backends() -> Vec<Box<dyn AudioBackend>> {
    let mut list: Vec<Box<dyn AudioBackend>> = Vec::with_capacity(3);

    #[cfg(feature = "pipewire")]
    let pw_avail = pipewire_runtime_available();
    #[cfg(feature = "pulseaudio")]
    let pulse_avail = pulseaudio_runtime_available();
    #[cfg(feature = "alsa")]
    let alsa_avail = alsa_runtime_available();

    #[cfg(feature = "pipewire")]
    if pw_avail {
        list.push(Box::new(super::pipewire::PipewireBackend));
    }

    #[cfg(feature = "pulseaudio")]
    if pulse_avail {
        list.push(Box::new(super::pulse::PulseBackend));
    }

    #[cfg(feature = "alsa")]
    if alsa_avail {
        list.push(Box::new(super::alsa::AlsaBackend));
    }

    #[cfg(feature = "pipewire")]
    if !pw_avail {
        list.push(Box::new(super::pipewire::PipewireBackend));
    }

    #[cfg(feature = "pulseaudio")]
    if !pulse_avail {
        list.push(Box::new(super::pulse::PulseBackend));
    }

    #[cfg(feature = "alsa")]
    if !alsa_avail {
        list.push(Box::new(super::alsa::AlsaBackend));
    }

    list
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    use super::{
        AudioCommand, SharedAudioState, StreamMetadata, first_non_empty_prop_by,
        stream_display_name, update_snapshot,
    };
    use crate::audio::types::{
        AudioCard, AudioChannelPosition, AudioDevice, AudioDirection, AudioEvent, AudioSnapshot,
        AudioStream, AudioTarget, AudioVolume,
    };

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
    fn stream_metadata_extract_normalizes_props() {
        let meta = StreamMetadata::extract(Some("raw-node"), |key| match key {
            "application.name" => Some("  Mozilla Firefox  "),
            "application.process.binary" => Some("firefox"),
            "media.role" => Some("video"),
            _ => None,
        });
        assert_eq!(meta.name, "Mozilla Firefox");
        assert_eq!(meta.app_name.as_deref(), Some("Mozilla Firefox"));
        assert_eq!(meta.binary_name.as_deref(), Some("firefox"));
        assert_eq!(meta.media_role.as_deref(), Some("video"));

        let fallback = first_non_empty_prop_by(&["a", "b"], |k| match k {
            "a" => Some("   "),
            "b" => Some("ok"),
            _ => None,
        });
        assert_eq!(fallback.as_deref(), Some("ok"));
    }

    #[test]
    fn orthogonal_commands_clone_with_targets() {
        let cmds = [
            AudioCommand::SetVolume(AudioTarget::PlaybackStream(7), 0.5),
            AudioCommand::SetMute(AudioTarget::PlaybackStream(7), true),
            AudioCommand::ToggleMute(AudioTarget::PlaybackStream(7)),
            AudioCommand::AdjustVolume(AudioTarget::CaptureStream(9), 0.1),
            AudioCommand::SetMute(AudioTarget::Sink(2), false),
        ];
        let cloned = cmds.clone();
        assert!(matches!(
            cloned[0],
            AudioCommand::SetVolume(AudioTarget::PlaybackStream(7), v) if (v - 0.5).abs() < 1e-6
        ));
        assert!(matches!(
            cloned[3],
            AudioCommand::AdjustVolume(AudioTarget::CaptureStream(9), d) if (d - 0.1).abs() < 1e-6
        ));
    }

    fn make_vol(linear: f32, muted: bool) -> AudioVolume {
        AudioVolume {
            linear,
            channels: vec![linear, linear],
            channel_map: vec![
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontRight,
            ],
            db: None,
            muted,
        }
    }

    #[test]
    fn update_snapshot_emits_granular_events_for_sinks_sources_ports_and_cards() {
        let shared = Arc::new(Mutex::new(SharedAudioState::default()));
        let mut last: Option<Arc<AudioSnapshot>> = None;

        let first = AudioSnapshot {
            sinks: vec![AudioDevice {
                id: 1,
                name: "sink-a".into(),
                description: "Sink A".into(),
                is_default: true,
                volume: make_vol(0.8, false),
                active_port: Some("lineout".into()),
                ports: Vec::new(),
            }],
            sources: vec![AudioDevice {
                id: 10,
                name: "mic-a".into(),
                description: "Mic A".into(),
                is_default: true,
                volume: make_vol(0.5, false),
                active_port: Some("front-mic".into()),
                ports: Vec::new(),
            }],
            default_sink_id: Some(1),
            default_source_id: Some(10),
            streams: vec![AudioStream {
                id: 3,
                direction: AudioDirection::Output,
                name: "Firefox".into(),
                volume: make_vol(1.0, false),
                ..AudioStream::default()
            }],
            cards: vec![AudioCard {
                id: 45,
                name: "card0".into(),
                description: "Card 0".into(),
                active_profile: Some("analog-stereo".into()),
                profiles: Vec::new(),
            }],
        };

        update_snapshot(&shared, &mut last, first.clone());
        {
            let mut g = shared.lock().unwrap();
            assert!(g.has_pending.load(Ordering::Acquire));
            assert_eq!(g.pending_snapshot.as_deref(), Some(&first));
            let evs: Vec<_> = g.pending_events.drain(..).collect();
            assert_eq!(
                evs,
                vec![
                    AudioEvent::DeviceAdded {
                        direction: AudioDirection::Output,
                        id: 1,
                        name: "sink-a".into(),
                    },
                    AudioEvent::DeviceAdded {
                        direction: AudioDirection::Input,
                        id: 10,
                        name: "mic-a".into(),
                    },
                ]
            );
            g.pending_snapshot = None;
            g.has_pending.store(false, Ordering::Release);
        }

        // Identical snapshot must be skipped without marking pending.
        update_snapshot(&shared, &mut last, first.clone());
        assert!(!shared.lock().unwrap().has_pending.load(Ordering::Acquire));

        // Update volume, port, card profile, remove source 10, add source 11.
        let mut second = first;
        second.sinks[0].volume = make_vol(0.5, false);
        second.sinks[0].active_port = Some("headphones".into());
        second.sources = vec![AudioDevice {
            id: 11,
            name: "usb-mic".into(),
            description: "USB Mic".into(),
            is_default: true,
            volume: make_vol(0.9, false),
            active_port: None,
            ports: Vec::new(),
        }];
        second.default_source_id = Some(11);
        second.cards[0].active_profile = Some("pro-audio".into());

        update_snapshot(&shared, &mut last, second);
        let g = shared.lock().unwrap();
        let evs: Vec<_> = g.pending_events.iter().cloned().collect();
        assert!(evs.contains(&AudioEvent::PortChanged {
            direction: AudioDirection::Output,
            device_id: 1,
            active_port: Some("headphones".into()),
        }));
        assert!(evs.contains(&AudioEvent::DeviceRemoved {
            direction: AudioDirection::Input,
            id: 10,
        }));
        assert!(evs.contains(&AudioEvent::DeviceAdded {
            direction: AudioDirection::Input,
            id: 11,
            name: "usb-mic".into(),
        }));
        assert!(evs.contains(&AudioEvent::DefaultDeviceChanged {
            direction: AudioDirection::Input,
            name: Some("usb-mic".into()),
        }));
        assert!(evs.contains(&AudioEvent::VolumeChanged {
            direction: AudioDirection::Output,
            volume: make_vol(0.5, false),
        }));
        assert!(evs.contains(&AudioEvent::VolumeChanged {
            direction: AudioDirection::Input,
            volume: make_vol(0.9, false),
        }));
        assert!(evs.contains(&AudioEvent::CardProfileChanged {
            card_id: 45,
            active_profile: Some("pro-audio".into()),
        }));
    }
}
