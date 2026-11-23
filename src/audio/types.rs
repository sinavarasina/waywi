use crate::subscription::{Subscription, SubscriptionKey, SubscriptionSpec};

/// Jack / port plug availability reported by the audio server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioPortAvailability {
    #[default]
    Unknown,
    Available,
    Unavailable,
}

impl AudioPortAvailability {
    /// Returns `true` unless the port is explicitly reported as [`Self::Unavailable`].
    #[must_use]
    pub const fn is_available(self) -> bool {
        !matches!(self, Self::Unavailable)
    }
}

/// Physical or logical port/route on a sink or source (e.g. `"analog-output-headphones"`,
/// `"analog-output-lineout"`, `"analog-input-front-mic"`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioPort {
    /// Backend route/port index (PipeWire `SPA_PARAM_ROUTE_index`; `0` on PulseAudio).
    pub index: i32,
    /// Port identifier name used with [`super::AudioClient::select_port`].
    pub name: String,
    /// Human-readable port label (e.g. `"Headphones"`, `"Line Out"`).
    pub description: String,
    /// Selection priority reported by the audio server (higher = preferred).
    pub priority: u32,
    /// Jack / port plug availability state.
    pub availability: AudioPortAvailability,
}

/// Profile supported by an [`AudioCard`] (e.g. `"output:analog-stereo+input:analog-stereo"`,
/// `"pro-audio"`, `"off"`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioProfile {
    /// Backend profile index (PipeWire `SPA_PARAM_PROFILE_index`; `0` on PulseAudio).
    pub index: i32,
    /// Profile identifier name used with [`super::AudioClient::select_card_profile`].
    pub name: String,
    /// Human-readable profile description (e.g. `"Analog Stereo Duplex"`).
    pub description: String,
    /// Selection priority reported by the audio server.
    pub priority: u32,
    /// Whether this profile is currently available for activation.
    pub available: bool,
}

/// Sound card device exposing switchable [`AudioProfile`] configurations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioCard {
    /// Backend card/device ID (PipeWire `Device` object ID; PulseAudio card index).
    pub id: u32,
    /// Card identifier name (e.g. `"alsa_card.pci-0000_0c_00.6"`).
    pub name: String,
    /// Human-readable card description (e.g. `"Ryzen HD Audio Controller"`).
    pub description: String,
    /// Currently active profile name on this card, when reported.
    pub active_profile: Option<String>,
    /// All profiles supported by this card.
    pub profiles: Vec<AudioProfile>,
}

/// Audio node metadata for an output sink or input source device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioDevice {
    pub id: u32,
    pub name: String,
    pub description: String,
    /// Owning [`AudioCard::id`], when backed by a hardware/card device.
    pub card_id: Option<u32>,
    /// Currently active port/route name on this device (e.g. `"analog-output-headphones"`).
    pub active_port: Option<String>,
    /// Switchable ports/routes available on this device under the active card profile.
    pub ports: Vec<AudioPort>,
}

/// Per-application playback stream routed into a sink (PulseAudio sink input, PipeWire
/// `Stream/Output/Audio` node).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioStream {
    pub id: u32,
    /// Human-readable application/stream name (`application.name` -> binary -> raw node name).
    pub name: String,
    /// Executable binary name (`application.process.binary`), when reported.
    pub binary: Option<String>,
    /// XDG / theme icon hint (`application.icon_name` or `application.icon-name`), when reported.
    pub icon_name: Option<String>,
    /// Application / Flatpak portal identifier (`application.id` or `pipewire.access.portal.app_id`), when reported.
    pub app_id: Option<String>,
    pub sink_id: Option<u32>,
    pub linear: f32,
    /// Per-channel linear volumes (`[left, right, ...]`).
    pub channels: Vec<f32>,
    pub db: Option<f32>,
    pub muted: bool,
}

impl AudioStream {
    /// Stereo L/R balance in `-1.0..=1.0` (`-1.0` = full left, `0.0` = center/mono, `+1.0` = full right).
    #[must_use]
    pub fn balance(&self) -> f32 {
        compute_stereo_balance(&self.channels)
    }
}

/// Per-application capture stream recorded from a source (PulseAudio source output, PipeWire
/// `Stream/Input/Audio` node).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioCapture {
    pub id: u32,
    /// Human-readable application/stream name (`application.name` -> binary -> raw node name).
    pub name: String,
    /// Executable binary name (`application.process.binary`), when reported.
    pub binary: Option<String>,
    /// XDG / theme icon hint (`application.icon_name` or `application.icon-name`), when reported.
    pub icon_name: Option<String>,
    /// Application / Flatpak portal identifier (`application.id` or `pipewire.access.portal.app_id`), when reported.
    pub app_id: Option<String>,
    pub source_id: Option<u32>,
    pub linear: f32,
    /// Per-channel linear volumes (`[left, right, ...]`).
    pub channels: Vec<f32>,
    pub db: Option<f32>,
    pub muted: bool,
}

impl AudioCapture {
    /// Stereo L/R balance in `-1.0..=1.0` (`-1.0` = full left, `0.0` = center/mono, `+1.0` = full right).
    #[must_use]
    pub fn balance(&self) -> f32 {
        compute_stereo_balance(&self.channels)
    }
}

/// Point-in-time snapshot of the default audio sink, default source (mic), available devices,
/// sound cards/profiles, and per-application playback/capture streams.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioSnapshot {
    pub linear: f32,
    /// Per-channel linear volumes of the active default sink (`[left, right, ...]`).
    pub channels: Vec<f32>,
    pub db: Option<f32>,
    pub muted: bool,
    pub default_sink: Option<AudioDevice>,
    pub devices: Vec<AudioDevice>,
    pub source_linear: f32,
    /// Per-channel linear volumes of the active default source (`[left, right, ...]`).
    pub source_channels: Vec<f32>,
    pub source_db: Option<f32>,
    pub source_muted: bool,
    pub default_source: Option<AudioDevice>,
    pub sources: Vec<AudioDevice>,
    /// Discovered sound cards and their switchable profiles.
    pub cards: Vec<AudioCard>,
    pub streams: Vec<AudioStream>,
    pub captures: Vec<AudioCapture>,
}

impl AudioSnapshot {
    /// Output sink volume expressed as percentage (`100.0` at unity gain).
    #[must_use]
    pub fn percent(&self) -> f32 {
        (self.linear * 100.0).clamp(0.0, 150.0)
    }

    /// Input source (microphone) volume expressed as percentage (`100.0` at unity gain).
    #[must_use]
    pub fn source_percent(&self) -> f32 {
        (self.source_linear * 100.0).clamp(0.0, 150.0)
    }

    /// Stereo L/R balance of the active default sink in `-1.0..=1.0` (`0.0` when centered or mono).
    #[must_use]
    pub fn balance(&self) -> f32 {
        compute_stereo_balance(&self.channels)
    }

    /// Stereo L/R balance of the active default source in `-1.0..=1.0` (`0.0` when centered or mono).
    #[must_use]
    pub fn source_balance(&self) -> f32 {
        compute_stereo_balance(&self.source_channels)
    }

    /// Ports available on the active default output sink.
    #[must_use]
    pub fn ports(&self) -> &[AudioPort] {
        self.default_sink
            .as_ref()
            .map_or(&[], |d| d.ports.as_slice())
    }

    /// Currently active [`AudioPort`] on the default output sink, if any.
    #[must_use]
    pub fn active_port(&self) -> Option<&AudioPort> {
        let sink = self.default_sink.as_ref()?;
        let active = sink.active_port.as_deref()?;
        sink.ports.iter().find(|p| p.name == active)
    }

    /// Ports available on the active default input source.
    #[must_use]
    pub fn source_ports(&self) -> &[AudioPort] {
        self.default_source
            .as_ref()
            .map_or(&[], |d| d.ports.as_slice())
    }

    /// Currently active [`AudioPort`] on the default input source, if any.
    #[must_use]
    pub fn active_source_port(&self) -> Option<&AudioPort> {
        let src = self.default_source.as_ref()?;
        let active = src.active_port.as_deref()?;
        src.ports.iter().find(|p| p.name == active)
    }

    /// Looks up an [`AudioCard`] by its backend `card_id`.
    #[must_use]
    pub fn card(&self, card_id: u32) -> Option<&AudioCard> {
        self.cards.iter().find(|c| c.id == card_id)
    }

    /// Returns the [`AudioCard`] backing [`Self::default_sink`], if known.
    #[must_use]
    pub fn default_sink_card(&self) -> Option<&AudioCard> {
        self.default_sink
            .as_ref()
            .and_then(|d| d.card_id)
            .and_then(|id| self.card(id))
    }

    /// Returns the [`AudioCard`] backing [`Self::default_source`], if known.
    #[must_use]
    pub fn default_source_card(&self) -> Option<&AudioCard> {
        self.default_source
            .as_ref()
            .and_then(|d| d.card_id)
            .and_then(|id| self.card(id))
    }

    /// Playback stream volume expressed as percentage (`100.0` at unity gain); `0.0` for an
    /// unknown stream ID.
    #[must_use]
    pub fn stream_percent(&self, id: u32) -> f32 {
        self.streams
            .iter()
            .find(|s| s.id == id)
            .map_or(0.0, |s| (s.linear * 100.0).clamp(0.0, 150.0))
    }

    /// Capture stream volume expressed as percentage (`100.0` at unity gain); `0.0` for an
    /// unknown stream ID.
    #[must_use]
    pub fn capture_percent(&self, id: u32) -> f32 {
        self.captures
            .iter()
            .find(|c| c.id == id)
            .map_or(0.0, |c| (c.linear * 100.0).clamp(0.0, 150.0))
    }
}

/// Computes stereo L/R balance in `-1.0..=1.0` from per-channel linear volumes (`[left, right, ..]`).
///
/// Returns `0.0` when fewer than 2 channels are present or both channels are silent.
#[must_use]
pub fn compute_stereo_balance(channels: &[f32]) -> f32 {
    if channels.len() < 2 {
        return 0.0;
    }
    let l = if channels[0].is_finite() {
        channels[0].max(0.0)
    } else {
        0.0
    };
    let r = if channels[1].is_finite() {
        channels[1].max(0.0)
    } else {
        0.0
    };
    let max = l.max(r);
    if max <= 1e-6 {
        0.0
    } else {
        ((r - l) / max).clamp(-1.0, 1.0)
    }
}

/// Scales `current` per-channel linear volumes so their peak equals `target_linear`, preserving
/// relative stereo/multi-channel balance ratios (`pa_cvolume_scale` semantics).
///
/// If `current` is empty or all-zero, falls back to `nonzero_template` when its length matches,
/// or a uniform vector of length `fallback_count.max(1)`.
#[must_use]
pub(crate) fn scale_linear_channels(
    current: &[f32],
    nonzero_template: &[f32],
    fallback_count: usize,
    target_linear: f32,
) -> Vec<f32> {
    let target = if target_linear.is_finite() {
        target_linear.clamp(0.0, 1.5)
    } else {
        0.0
    };
    let count = if !current.is_empty() {
        current.len()
    } else if !nonzero_template.is_empty() {
        nonzero_template.len()
    } else {
        fallback_count.max(1)
    };
    let cur_max = current
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0_f32, f32::max);
    if cur_max > 1e-6 {
        return current
            .iter()
            .map(|&v| {
                let clean = if v.is_finite() { v.max(0.0) } else { 0.0 };
                ((clean / cur_max) * target).clamp(0.0, 1.5)
            })
            .collect();
    }
    let tpl_max = nonzero_template
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0_f32, f32::max);
    if nonzero_template.len() == count && tpl_max > 1e-6 {
        return nonzero_template
            .iter()
            .map(|&v| {
                let clean = if v.is_finite() { v.max(0.0) } else { 0.0 };
                ((clean / tpl_max) * target).clamp(0.0, 1.5)
            })
            .collect();
    }
    vec![target; count]
}

/// Applies a stereo L/R balance (`-1.0..=1.0`) to `current` per-channel linear volumes while
/// preserving their peak master level (`fallback_linear` when all channels are silent).
#[must_use]
pub(crate) fn apply_linear_balance(
    current: &[f32],
    fallback_count: usize,
    fallback_linear: f32,
    balance: f32,
) -> Vec<f32> {
    let b = if balance.is_finite() {
        balance.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let count = if current.is_empty() {
        fallback_count.max(1)
    } else {
        current.len()
    };
    let cur_max = current
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0_f32, f32::max);
    let master = if cur_max > 1e-6 {
        cur_max
    } else if fallback_linear.is_finite() {
        fallback_linear.clamp(0.0, 1.5)
    } else {
        0.0
    };
    if count == 1 {
        return vec![master];
    }
    let left_gain = if b > 0.0 { 1.0 - b } else { 1.0 };
    let right_gain = if b < 0.0 { 1.0 + b } else { 1.0 };
    (0..count)
        .map(|idx| {
            let gain = if idx % 2 == 0 { left_gain } else { right_gain };
            (master * gain).clamp(0.0, 1.5)
        })
        .collect()
}

/// Sanitizes an explicit per-channel linear volume slice, clamping each finite channel to
/// `[0.0, 1.5]`. Returns `None` if `volumes` is empty.
#[must_use]
pub(crate) fn sanitize_linear_channels(volumes: &[f32]) -> Option<Vec<f32>> {
    if volumes.is_empty() {
        return None;
    }
    Some(
        volumes
            .iter()
            .map(|&v| {
                if v.is_finite() {
                    v.clamp(0.0, 1.5)
                } else {
                    0.0
                }
            })
            .collect(),
    )
}

/// Event emitted by the audio monitor.
#[non_exhaustive]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum AudioEvent {
    Updated(AudioSnapshot),
    DeviceAdded(AudioDevice),
    DeviceRemoved(u32),
    Disconnected,
    Reconnected,
}

/// Audio subscription constructors.
pub struct Audio;

impl Audio {
    /// Subscribes to default sink/source volume, mute, and device snapshots whenever audio state
    /// changes.
    pub fn subscribe<M: 'static>(map: impl Fn(AudioSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::AudioSnapshot {
                key: SubscriptionKey::from_hashable(&(
                    "audio-snapshot",
                    std::any::TypeId::of::<M>(),
                )),
                map: crate::arena::FrameSharedFn::new(map),
            },
        }
    }

    /// Subscribes to audio events including connect/disconnect and device hotplug transitions.
    pub fn subscribe_events<M: 'static>(
        map: impl Fn(AudioEvent) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::AudioEvent {
                key: SubscriptionKey::from_hashable(&("audio-event", std::any::TypeId::of::<M>())),
                map: crate::arena::FrameSharedFn::new(map),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AudioCapture, AudioCard, AudioDevice, AudioPort, AudioPortAvailability, AudioProfile,
        AudioSnapshot, AudioStream, apply_linear_balance, compute_stereo_balance,
        sanitize_linear_channels, scale_linear_channels,
    };

    #[test]
    fn percent_helpers_map_unity_to_100_and_unknown_to_zero() {
        let snap = AudioSnapshot {
            linear: 1.5,
            streams: vec![AudioStream {
                id: 3,
                name: "Firefox".into(),
                binary: None,
                icon_name: None,
                app_id: None,
                sink_id: None,
                linear: 0.5,
                channels: vec![0.5, 0.5],
                db: None,
                muted: false,
            }],
            captures: vec![AudioCapture {
                id: 5,
                name: "Recorder".into(),
                binary: Some("rec".into()),
                icon_name: None,
                app_id: None,
                source_id: None,
                linear: 0.75,
                channels: vec![0.75],
                db: None,
                muted: true,
            }],
            ..AudioSnapshot::default()
        };

        assert_eq!(snap.percent(), 150.0);
        assert_eq!(snap.stream_percent(3), 50.0);
        assert_eq!(snap.stream_percent(4), 0.0);
        assert_eq!(snap.capture_percent(5), 75.0);
        assert_eq!(snap.capture_percent(6), 0.0);
        assert_eq!(snap.source_percent(), 0.0);
    }

    #[test]
    fn stereo_balance_and_channel_scaling_preserve_proportions() {
        assert_eq!(compute_stereo_balance(&[]), 0.0);
        assert_eq!(compute_stereo_balance(&[0.8]), 0.0);
        assert_eq!(compute_stereo_balance(&[0.0, 0.0]), 0.0);
        assert_eq!(compute_stereo_balance(&[0.8, 0.8]), 0.0);
        assert!((compute_stereo_balance(&[0.8, 0.4]) - (-0.5)).abs() < 1e-5);
        assert!((compute_stereo_balance(&[0.3, 0.6]) - 0.5).abs() < 1e-5);

        // Scaling preserves 2:1 L/R balance ratio.
        let scaled = scale_linear_channels(&[0.8, 0.4], &[0.8, 0.4], 2, 0.6);
        assert!((scaled[0] - 0.6).abs() < 1e-5);
        assert!((scaled[1] - 0.3).abs() < 1e-5);
        assert!((compute_stereo_balance(&scaled) - (-0.5)).abs() < 1e-5);

        // Scaling back up from 0.0 recovers the non-zero template ratios.
        let muted_zero = [0.0, 0.0];
        let recovered = scale_linear_channels(&muted_zero, &[0.8, 0.4], 2, 0.8);
        assert!((recovered[0] - 0.8).abs() < 1e-5);
        assert!((recovered[1] - 0.4).abs() < 1e-5);

        // Applying balance adjusts L/R while keeping peak master level.
        let left_biased = apply_linear_balance(&[0.8, 0.8], 2, 0.8, -0.5);
        assert!((left_biased[0] - 0.8).abs() < 1e-5);
        assert!((left_biased[1] - 0.4).abs() < 1e-5);
        let right_biased = apply_linear_balance(&left_biased, 2, 0.8, 0.25);
        assert!((right_biased[0] - 0.6).abs() < 1e-5);
        assert!((right_biased[1] - 0.8).abs() < 1e-5);

        assert_eq!(sanitize_linear_channels(&[]), None);
        assert_eq!(
            sanitize_linear_channels(&[-0.2, 0.5, 2.0, f32::NAN]),
            Some(vec![0.0, 0.5, 1.5, 0.0])
        );
    }

    #[test]
    fn snapshot_exposes_active_ports_and_cards() {
        let hp_port = AudioPort {
            index: 4,
            name: "analog-output-headphones".into(),
            description: "Headphones".into(),
            priority: 9900,
            availability: AudioPortAvailability::Available,
        };
        let lo_port = AudioPort {
            index: 3,
            name: "analog-output-lineout".into(),
            description: "Line Out".into(),
            priority: 9000,
            availability: AudioPortAvailability::Available,
        };
        let sink = AudioDevice {
            id: 57,
            name: "alsa_output.alc1220".into(),
            description: "ALC1220 Analog".into(),
            card_id: Some(45),
            active_port: Some("analog-output-headphones".into()),
            ports: vec![hp_port.clone(), lo_port],
        };
        let card = AudioCard {
            id: 45,
            name: "alsa_card.pci-0000_0c_00.6".into(),
            description: "Ryzen HD Audio Controller".into(),
            active_profile: Some("output:analog-stereo+input:analog-stereo".into()),
            profiles: vec![AudioProfile {
                index: 1,
                name: "output:analog-stereo+input:analog-stereo".into(),
                description: "Analog Stereo Duplex".into(),
                priority: 6565,
                available: true,
            }],
        };
        let snap = AudioSnapshot {
            linear: 0.8,
            channels: vec![0.8, 0.4],
            default_sink: Some(sink.clone()),
            devices: vec![sink],
            cards: vec![card.clone()],
            ..AudioSnapshot::default()
        };
        assert!((snap.balance() - (-0.5)).abs() < 1e-5);
        assert_eq!(snap.active_port(), Some(&hp_port));
        assert_eq!(snap.ports().len(), 2);
        assert_eq!(snap.default_sink_card(), Some(&card));
        assert!(hp_port.availability.is_available());
        assert!(!AudioPortAvailability::Unavailable.is_available());
    }
}
