use std::sync::Arc;

use crate::arena::FrameSharedFn;
use crate::subscription::{Subscription, SubscriptionSpec};

/// Signal flow direction for an audio device, stream, or default-device query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AudioDirection {
    /// Playback / output direction (sinks and playback streams).
    #[default]
    Output,
    /// Capture / input direction (sources/microphones and recording streams).
    Input,
}

/// Target selector for orthogonal volume, balance, channel, and mute operations on
/// [`super::AudioClient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioTarget {
    /// Currently active default output sink.
    DefaultSink,
    /// Currently active default input source (microphone).
    DefaultSource,
    /// Specific output sink identified by backend device/node ID.
    Sink(u32),
    /// Specific input source identified by backend device/node ID.
    Source(u32),
    /// Per-application playback stream identified by backend stream/node ID.
    PlaybackStream(u32),
    /// Per-application capture stream identified by backend stream/node ID.
    CaptureStream(u32),
}

/// Target selector for switching physical or logical ports/routes on a sink or source via
/// [`super::AudioClient::set_port`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioPortTarget {
    /// Currently active default output sink.
    DefaultSink,
    /// Currently active default input source (microphone).
    DefaultSource,
    /// Specific output sink identified by backend device/node ID.
    Sink(u32),
    /// Specific input source identified by backend device/node ID.
    Source(u32),
}

/// Spatial speaker / channel position reported by the audio server (`SPA_PROP_channelMap` on
/// PipeWire, `pa_channel_map` on PulseAudio, `SelemChannelId` on ALSA).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AudioChannelPosition {
    #[default]
    Unknown,
    Mono,
    FrontLeft,
    FrontRight,
    FrontCenter,
    Lfe,
    RearLeft,
    RearRight,
    RearCenter,
    SideLeft,
    SideRight,
    FrontLeftCenter,
    FrontRightCenter,
    TopCenter,
    TopFrontLeft,
    TopFrontCenter,
    TopFrontRight,
    TopRearLeft,
    TopRearCenter,
    TopRearRight,
    Aux(u16),
}

impl AudioChannelPosition {
    /// Returns `true` if this speaker position belongs to the left stereo/surround hemisphere.
    #[must_use]
    pub const fn is_left(self) -> bool {
        matches!(
            self,
            Self::FrontLeft
                | Self::RearLeft
                | Self::SideLeft
                | Self::FrontLeftCenter
                | Self::TopFrontLeft
                | Self::TopRearLeft
        )
    }

    /// Returns `true` if this speaker position belongs to the right stereo/surround hemisphere.
    #[must_use]
    pub const fn is_right(self) -> bool {
        matches!(
            self,
            Self::FrontRight
                | Self::RearRight
                | Self::SideRight
                | Self::FrontRightCenter
                | Self::TopFrontRight
                | Self::TopRearRight
        )
    }

    /// Returns `true` if this speaker position lies on the center plane or is an LFE/subwoofer
    /// channel (unaffected by left/right balance panning).
    #[must_use]
    pub const fn is_center_or_lfe(self) -> bool {
        matches!(
            self,
            Self::Mono
                | Self::FrontCenter
                | Self::Lfe
                | Self::RearCenter
                | Self::TopCenter
                | Self::TopFrontCenter
                | Self::TopRearCenter
        )
    }

    /// Returns `true` if this speaker position belongs to the front soundstage.
    #[must_use]
    pub const fn is_front(self) -> bool {
        matches!(
            self,
            Self::FrontLeft
                | Self::FrontRight
                | Self::FrontCenter
                | Self::FrontLeftCenter
                | Self::FrontRightCenter
                | Self::TopFrontLeft
                | Self::TopFrontCenter
                | Self::TopFrontRight
        )
    }

    /// Returns `true` if this speaker position belongs to the rear soundstage.
    #[must_use]
    pub const fn is_rear(self) -> bool {
        matches!(
            self,
            Self::RearLeft
                | Self::RearRight
                | Self::RearCenter
                | Self::TopRearLeft
                | Self::TopRearCenter
                | Self::TopRearRight
        )
    }

    /// Short standard label for UI display (`"FL"`, `"FR"`, `"FC"`, `"LFE"`, `"RL"`, `"RR"`, etc.).
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Unknown => "UNK",
            Self::Mono => "MONO",
            Self::FrontLeft => "FL",
            Self::FrontRight => "FR",
            Self::FrontCenter => "FC",
            Self::Lfe => "LFE",
            Self::RearLeft => "RL",
            Self::RearRight => "RR",
            Self::RearCenter => "RC",
            Self::SideLeft => "SL",
            Self::SideRight => "SR",
            Self::FrontLeftCenter => "FLC",
            Self::FrontRightCenter => "FRC",
            Self::TopCenter => "TC",
            Self::TopFrontLeft => "TFL",
            Self::TopFrontCenter => "TFC",
            Self::TopFrontRight => "TFR",
            Self::TopRearLeft => "TRL",
            Self::TopRearCenter => "TRC",
            Self::TopRearRight => "TRR",
            Self::Aux(_) => "AUX",
        }
    }

    /// Synthesizes the standard ITU/ALSA/PipeWire channel map for `count` channels (`1` = Mono,
    /// `2` = Stereo, `4` = Quad, `6` = 5.1 Surround, `8` = 7.1 Surround).
    #[must_use]
    pub fn default_map_for_count(count: usize) -> Vec<Self> {
        (0..count)
            .map(|idx| Self::default_for_index(idx, count))
            .collect()
    }

    #[must_use]
    pub(crate) const fn default_for_index(idx: usize, count: usize) -> Self {
        match count {
            0 | 1 => Self::Mono,
            2 => match idx {
                0 => Self::FrontLeft,
                _ => Self::FrontRight,
            },
            3 => match idx {
                0 => Self::FrontLeft,
                1 => Self::FrontRight,
                _ => Self::Lfe,
            },
            4 => match idx {
                0 => Self::FrontLeft,
                1 => Self::FrontRight,
                2 => Self::RearLeft,
                _ => Self::RearRight,
            },
            5 => match idx {
                0 => Self::FrontLeft,
                1 => Self::FrontRight,
                2 => Self::FrontCenter,
                3 => Self::RearLeft,
                _ => Self::RearRight,
            },
            6 => match idx {
                0 => Self::FrontLeft,
                1 => Self::FrontRight,
                2 => Self::FrontCenter,
                3 => Self::Lfe,
                4 => Self::RearLeft,
                _ => Self::RearRight,
            },
            7 => match idx {
                0 => Self::FrontLeft,
                1 => Self::FrontRight,
                2 => Self::FrontCenter,
                3 => Self::Lfe,
                4 => Self::RearCenter,
                5 => Self::SideLeft,
                _ => Self::SideRight,
            },
            _ => match idx {
                0 => Self::FrontLeft,
                1 => Self::FrontRight,
                2 => Self::FrontCenter,
                3 => Self::Lfe,
                4 => Self::RearLeft,
                5 => Self::RearRight,
                6 => Self::SideLeft,
                7 => Self::SideRight,
                other => Self::Aux((other - 8) as u16),
            },
        }
    }
}

/// Normalized volume, per-channel levels, spatial channel map, decibel level, and mute state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioVolume {
    /// Peak linear volume level (`1.0` = unity gain / 100%, up to `1.5` = 150% boost).
    pub linear: f32,
    /// Per-channel linear volumes (`[left, right, ...]`).
    pub channels: Vec<f32>,
    /// Spatial speaker positions corresponding to [`Self::channels`].
    pub channel_map: Vec<AudioChannelPosition>,
    /// Peak level in decibels when reported and audible.
    pub db: Option<f32>,
    /// Whether the endpoint or stream is currently muted.
    pub muted: bool,
}

impl AudioVolume {
    /// Volume expressed as percentage (`100.0` at unity gain, clamped to `0.0..=150.0`).
    #[must_use]
    pub fn percent(&self) -> f32 {
        if self.linear.is_finite() {
            (self.linear * 100.0).clamp(0.0, 150.0)
        } else {
            0.0
        }
    }

    /// Computes stereo/surround L/R balance in `-1.0..=1.0` from [`Self::channels`] and
    /// [`Self::channel_map`]. Returns `None` for mono or non-directional channel layouts.
    #[must_use]
    pub fn balance(&self) -> Option<f32> {
        compute_balance_opt(&self.channels, &self.channel_map)
    }

    /// Returns the number of channels tracked in [`Self::channels`].
    #[must_use]
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
}

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
    /// Port identifier name used with [`super::AudioClient::set_port`].
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
    /// Profile identifier name used with [`super::AudioClient::set_card_profile`].
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

/// Audio endpoint metadata and live volume state for an output sink or input source device.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioDevice {
    pub id: u32,
    pub name: String,
    pub description: String,
    /// Whether this device is currently the active default for its direction.
    pub is_default: bool,
    /// Current volume, channel layout, dB level, and mute state of this device.
    pub volume: AudioVolume,
    /// Currently active port/route name on this device (e.g. `"analog-output-headphones"`).
    pub active_port: Option<String>,
    /// Switchable ports/routes available on this device under the active card profile.
    pub ports: Vec<AudioPort>,
}

/// Unified per-application playback or capture stream (PulseAudio sink input / source output,
/// PipeWire `Stream/Output/Audio` or `Stream/Input/Audio` node).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioStream {
    pub id: u32,
    /// Whether this stream is playing back (`Output`) or capturing (`Input`).
    pub direction: AudioDirection,
    /// Human-readable stream/application label.
    pub name: String,
    /// Application display name (`application.name`), when reported.
    pub app_name: Option<String>,
    /// Executable binary name (`application.process.binary`), when reported.
    pub binary_name: Option<String>,
    /// Media role hint (`media.role`, e.g. `"music"`, `"phone"`, `"video"`), when reported.
    pub media_role: Option<String>,
    /// Backend sink/source ID this stream is currently routed to, when known.
    pub device_id: Option<u32>,
    /// Whether the stream is currently corked/paused/suspended.
    pub corked: bool,
    /// Current volume, channel layout, dB level, and mute state of this stream.
    pub volume: AudioVolume,
}

/// Point-in-time snapshot of all discovered output sinks, input sources, per-application streams,
/// and sound cards.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioSnapshot {
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
    pub default_sink_id: Option<u32>,
    pub default_source_id: Option<u32>,
    pub streams: Vec<AudioStream>,
    pub cards: Vec<AudioCard>,
}

impl AudioSnapshot {
    /// Returns the active default output sink, if present.
    #[must_use]
    pub fn default_sink(&self) -> Option<&AudioDevice> {
        self.default_sink_id
            .and_then(|id| self.sinks.iter().find(|d| d.id == id))
            .or_else(|| self.sinks.iter().find(|d| d.is_default))
    }

    /// Returns the active default input source (microphone), if present.
    #[must_use]
    pub fn default_source(&self) -> Option<&AudioDevice> {
        self.default_source_id
            .and_then(|id| self.sources.iter().find(|d| d.id == id))
            .or_else(|| self.sources.iter().find(|d| d.is_default))
    }

    /// Iterates over all per-application playback streams (`AudioDirection::Output`).
    pub fn playback_streams(&self) -> impl Iterator<Item = &AudioStream> {
        self.streams
            .iter()
            .filter(|s| s.direction == AudioDirection::Output)
    }

    /// Iterates over all per-application capture streams (`AudioDirection::Input`).
    pub fn capture_streams(&self) -> impl Iterator<Item = &AudioStream> {
        self.streams
            .iter()
            .filter(|s| s.direction == AudioDirection::Input)
    }
}

#[inline]
fn resolve_channel_position(
    channel_map: &[AudioChannelPosition],
    idx: usize,
    count: usize,
) -> AudioChannelPosition {
    match channel_map.get(idx).copied() {
        Some(pos) if pos != AudioChannelPosition::Unknown => pos,
        _ => AudioChannelPosition::default_for_index(idx, count),
    }
}

fn compute_balance_opt(channels: &[f32], channel_map: &[AudioChannelPosition]) -> Option<f32> {
    let count = channels.len();
    if count < 2 {
        return None;
    }
    let mut left_sum = 0.0_f32;
    let mut left_n = 0_u32;
    let mut right_sum = 0.0_f32;
    let mut right_n = 0_u32;

    for (idx, &raw) in channels.iter().enumerate() {
        let v = if raw.is_finite() { raw.max(0.0) } else { 0.0 };
        let pos = resolve_channel_position(channel_map, idx, count);
        if pos.is_left() {
            left_sum += v;
            left_n += 1;
        } else if pos.is_right() {
            right_sum += v;
            right_n += 1;
        }
    }

    if left_n == 0 || right_n == 0 {
        return None;
    }
    let l = left_sum / (left_n as f32);
    let r = right_sum / (right_n as f32);
    let max = l.max(r);
    if max <= 1e-6 {
        Some(0.0)
    } else {
        Some(((r - l) / max).clamp(-1.0, 1.0))
    }
}

/// Computes L/R balance in `-1.0..=1.0` from per-channel linear volumes using the standard ITU/ALSA
/// speaker map for `channels.len()` (`2` = Stereo, `4` = Quad, `6` = 5.1 Surround, `8` = 7.1 Surround).
///
/// Center and LFE channels are excluded from L/R balance calculation (`pa_cvolume_get_balance`
/// semantics). Returns `0.0` when fewer than 2 directional channels are present or silent.
#[must_use]
pub fn compute_stereo_balance(channels: &[f32]) -> f32 {
    compute_balance_with_map(channels, &[])
}

/// Computes L/R balance in `-1.0..=1.0` from per-channel linear volumes and an explicit
/// [`AudioChannelPosition`] map (falling back to the standard layout for `channels.len()` when
/// `channel_map` is empty or contains [`AudioChannelPosition::Unknown`]).
#[must_use]
pub fn compute_balance_with_map(channels: &[f32], channel_map: &[AudioChannelPosition]) -> f32 {
    compute_balance_opt(channels, channel_map).unwrap_or(0.0)
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

/// Applies a stereo/surround L/R balance (`-1.0..=1.0`) to `current` per-channel linear volumes
/// using the standard channel layout for `current.len()` (or `fallback_count`).
#[allow(dead_code)]
#[must_use]
pub(crate) fn apply_linear_balance(
    current: &[f32],
    fallback_count: usize,
    fallback_linear: f32,
    balance: f32,
) -> Vec<f32> {
    apply_linear_balance_with_map(current, &[], fallback_count, fallback_linear, balance)
}

/// Applies a stereo/surround L/R balance (`-1.0..=1.0`) to `current` per-channel linear volumes
/// with [`AudioChannelPosition`] awareness (`pa_cvolume_set_balance` semantics):
///
/// - Left channels (`FL`, `RL`, `SL`, ...) and right channels (`FR`, `RR`, `SR`, ...) are scaled
///   proportionally to their hemisphere average so front-to-rear fade ratios are preserved.
/// - Center, LFE/subwoofer, and auxiliary channels (`FC`, `LFE`, `RC`, ...) remain untouched at
///   their existing levels instead of being treated as left/right channels.
#[must_use]
pub(crate) fn apply_linear_balance_with_map(
    current: &[f32],
    channel_map: &[AudioChannelPosition],
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

    let mut left_sum = 0.0_f32;
    let mut left_n = 0_u32;
    let mut right_sum = 0.0_f32;
    let mut right_n = 0_u32;
    for idx in 0..count {
        let v = current
            .get(idx)
            .copied()
            .filter(|v| v.is_finite())
            .map_or(master, |v| v.max(0.0));
        let pos = resolve_channel_position(channel_map, idx, count);
        if pos.is_left() {
            left_sum += v;
            left_n += 1;
        } else if pos.is_right() {
            right_sum += v;
            right_n += 1;
        }
    }

    if left_n == 0 || right_n == 0 {
        return if current.is_empty() {
            vec![master; count]
        } else {
            current
                .iter()
                .map(|&v| {
                    if v.is_finite() {
                        v.clamp(0.0, 1.5)
                    } else {
                        0.0
                    }
                })
                .collect()
        };
    }

    let left_avg = left_sum / (left_n as f32);
    let right_avg = right_sum / (right_n as f32);
    let lr_ref = {
        let m = left_avg.max(right_avg);
        if m > 1e-6 { m } else { master }
    };
    let target_left = if b > 0.0 { lr_ref * (1.0 - b) } else { lr_ref };
    let target_right = if b < 0.0 { lr_ref * (1.0 + b) } else { lr_ref };

    (0..count)
        .map(|idx| {
            let cur_v = current
                .get(idx)
                .copied()
                .filter(|v| v.is_finite())
                .map_or(master, |v| v.max(0.0));
            let pos = resolve_channel_position(channel_map, idx, count);
            if pos.is_left() {
                let next = if left_avg > 1e-6 {
                    cur_v * (target_left / left_avg)
                } else {
                    target_left
                };
                next.clamp(0.0, 1.5)
            } else if pos.is_right() {
                let next = if right_avg > 1e-6 {
                    cur_v * (target_right / right_avg)
                } else {
                    target_right
                };
                next.clamp(0.0, 1.5)
            } else {
                cur_v.clamp(0.0, 1.5)
            }
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

/// Granular event emitted by the audio backend monitor.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum AudioEvent {
    DeviceAdded {
        direction: AudioDirection,
        id: u32,
        name: String,
    },
    DeviceRemoved {
        direction: AudioDirection,
        id: u32,
    },
    DefaultDeviceChanged {
        direction: AudioDirection,
        name: Option<String>,
    },
    VolumeChanged {
        direction: AudioDirection,
        volume: AudioVolume,
    },
    CardProfileChanged {
        card_id: u32,
        active_profile: Option<String>,
    },
    PortChanged {
        direction: AudioDirection,
        device_id: u32,
        active_port: Option<String>,
    },
    BackendDisconnected,
    BackendReconnected,
}

/// Audio subscription constructors.
pub struct Audio;

impl Audio {
    /// Subscribes to full audio state snapshots (`Arc<AudioSnapshot>`) whenever sinks, sources,
    /// streams, cards, volumes, or defaults change.
    pub fn subscribe<M: 'static>(
        map: impl Fn(Arc<AudioSnapshot>) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::AudioSnapshot {
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Subscribes to discrete audio lifecycle and device/volume change events.
    pub fn events<M: 'static>(map: impl Fn(AudioEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::AudioEvent {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AudioCard, AudioChannelPosition, AudioDevice, AudioDirection, AudioPort,
        AudioPortAvailability, AudioProfile, AudioSnapshot, AudioStream, AudioVolume,
        apply_linear_balance, apply_linear_balance_with_map, compute_balance_with_map,
        compute_stereo_balance, sanitize_linear_channels, scale_linear_channels,
    };

    #[test]
    fn volume_helpers_and_snapshot_accessors_work() {
        let sink = AudioDevice {
            id: 10,
            name: "alsa_output.pci".into(),
            description: "Built-in Output".into(),
            is_default: true,
            volume: AudioVolume {
                linear: 1.5,
                channels: vec![1.5, 0.75],
                channel_map: vec![
                    AudioChannelPosition::FrontLeft,
                    AudioChannelPosition::FrontRight,
                ],
                db: Some(10.5),
                muted: false,
            },
            active_port: Some("analog-output-headphones".into()),
            ports: vec![AudioPort {
                index: 4,
                name: "analog-output-headphones".into(),
                description: "Headphones".into(),
                priority: 9900,
                availability: AudioPortAvailability::Available,
            }],
        };
        let source = AudioDevice {
            id: 20,
            name: "alsa_input.mic".into(),
            description: "Mic".into(),
            is_default: true,
            volume: AudioVolume {
                linear: 0.75,
                channels: vec![0.75],
                channel_map: vec![AudioChannelPosition::Mono],
                db: None,
                muted: true,
            },
            active_port: None,
            ports: Vec::new(),
        };
        let snap = AudioSnapshot {
            sinks: vec![sink.clone()],
            sources: vec![source.clone()],
            default_sink_id: Some(10),
            default_source_id: Some(20),
            streams: vec![
                AudioStream {
                    id: 3,
                    direction: AudioDirection::Output,
                    name: "Firefox".into(),
                    app_name: Some("Firefox".into()),
                    binary_name: Some("firefox".into()),
                    media_role: Some("video".into()),
                    device_id: Some(10),
                    corked: false,
                    volume: AudioVolume {
                        linear: 0.5,
                        channels: vec![0.5, 0.5],
                        channel_map: vec![
                            AudioChannelPosition::FrontLeft,
                            AudioChannelPosition::FrontRight,
                        ],
                        db: None,
                        muted: false,
                    },
                },
                AudioStream {
                    id: 5,
                    direction: AudioDirection::Input,
                    name: "Recorder".into(),
                    app_name: Some("Recorder".into()),
                    binary_name: Some("rec".into()),
                    media_role: None,
                    device_id: Some(20),
                    corked: true,
                    volume: AudioVolume {
                        linear: 0.75,
                        channels: vec![0.75],
                        channel_map: vec![AudioChannelPosition::Mono],
                        db: None,
                        muted: true,
                    },
                },
            ],
            cards: vec![AudioCard {
                id: 45,
                name: "alsa_card.pci".into(),
                description: "HD Audio".into(),
                active_profile: Some("output:analog-stereo".into()),
                profiles: vec![AudioProfile {
                    index: 1,
                    name: "output:analog-stereo".into(),
                    description: "Analog Stereo".into(),
                    priority: 6565,
                    available: true,
                }],
            }],
        };

        assert_eq!(snap.default_sink(), Some(&sink));
        assert_eq!(snap.default_source(), Some(&source));
        assert_eq!(sink.volume.percent(), 150.0);
        assert_eq!(sink.volume.channel_count(), 2);
        assert!((sink.volume.balance().unwrap() - (-0.5)).abs() < 1e-5);
        assert_eq!(source.volume.percent(), 75.0);
        assert_eq!(source.volume.channel_count(), 1);
        assert_eq!(source.volume.balance(), None);

        let playback: Vec<u32> = snap.playback_streams().map(|s| s.id).collect();
        let capture: Vec<u32> = snap.capture_streams().map(|s| s.id).collect();
        assert_eq!(playback, vec![3]);
        assert_eq!(capture, vec![5]);
        assert!(sink.ports[0].availability.is_available());
        assert!(!AudioPortAvailability::Unavailable.is_available());
    }

    #[test]
    fn stereo_balance_and_channel_scaling_preserve_proportions() {
        assert_eq!(compute_stereo_balance(&[]), 0.0);
        assert_eq!(compute_stereo_balance(&[0.8]), 0.0);
        assert_eq!(compute_stereo_balance(&[0.0, 0.0]), 0.0);
        assert_eq!(compute_stereo_balance(&[0.8, 0.8]), 0.0);
        assert!((compute_stereo_balance(&[0.8, 0.4]) - (-0.5)).abs() < 1e-5);
        assert!((compute_stereo_balance(&[0.3, 0.6]) - 0.5).abs() < 1e-5);

        let scaled = scale_linear_channels(&[0.8, 0.4], &[0.8, 0.4], 2, 0.6);
        assert!((scaled[0] - 0.6).abs() < 1e-5);
        assert!((scaled[1] - 0.3).abs() < 1e-5);
        assert!((compute_stereo_balance(&scaled) - (-0.5)).abs() < 1e-5);

        let muted_zero = [0.0, 0.0];
        let recovered = scale_linear_channels(&muted_zero, &[0.8, 0.4], 2, 0.8);
        assert!((recovered[0] - 0.8).abs() < 1e-5);
        assert!((recovered[1] - 0.4).abs() < 1e-5);

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
    fn surround_5_1_and_7_1_balance_preserves_center_lfe_and_front_rear_fade() {
        let map_5_1 = AudioChannelPosition::default_map_for_count(6);
        assert_eq!(
            map_5_1,
            vec![
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontRight,
                AudioChannelPosition::FrontCenter,
                AudioChannelPosition::Lfe,
                AudioChannelPosition::RearLeft,
                AudioChannelPosition::RearRight,
            ]
        );
        let ch_5_1 = [0.8, 0.8, 0.7, 0.5, 0.6, 0.6];
        assert!((compute_balance_with_map(&ch_5_1, &map_5_1) - 0.0).abs() < 1e-5);

        let panned = apply_linear_balance_with_map(&ch_5_1, &map_5_1, 6, 0.8, -0.5);
        assert!((panned[0] - 0.8).abs() < 1e-5);
        assert!((panned[1] - 0.4).abs() < 1e-5);
        assert!((panned[2] - 0.7).abs() < 1e-5);
        assert!((panned[3] - 0.5).abs() < 1e-5);
        assert!((panned[4] - 0.6).abs() < 1e-5);
        assert!((panned[5] - 0.3).abs() < 1e-5);
        assert!((compute_balance_with_map(&panned, &map_5_1) - (-0.5)).abs() < 1e-5);

        let map_7_1 = AudioChannelPosition::default_map_for_count(8);
        let ch_7_1 = [0.8, 0.8, 0.75, 0.4, 0.8, 0.8, 0.8, 0.8];
        let panned_7_1 = apply_linear_balance_with_map(&ch_7_1, &map_7_1, 8, 0.8, 0.5);
        assert!((panned_7_1[0] - 0.4).abs() < 1e-5);
        assert!((panned_7_1[1] - 0.8).abs() < 1e-5);
        assert!((panned_7_1[2] - 0.75).abs() < 1e-5);
        assert!((panned_7_1[3] - 0.4).abs() < 1e-5);
        assert!((panned_7_1[4] - 0.4).abs() < 1e-5);
        assert!((panned_7_1[5] - 0.8).abs() < 1e-5);
        assert!((panned_7_1[6] - 0.4).abs() < 1e-5);
        assert!((panned_7_1[7] - 0.8).abs() < 1e-5);
        assert!((compute_balance_with_map(&panned_7_1, &map_7_1) - 0.5).abs() < 1e-5);
    }
}
