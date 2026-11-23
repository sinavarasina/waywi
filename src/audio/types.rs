use crate::subscription::{Subscription, SubscriptionKey, SubscriptionSpec};

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
    /// Spatial speaker/channel layout reported for this device (`[FrontLeft, FrontRight, ...]`).
    pub channel_map: Vec<AudioChannelPosition>,
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
    /// Spatial channel positions corresponding to [`Self::channels`].
    pub channel_map: Vec<AudioChannelPosition>,
    pub db: Option<f32>,
    pub muted: bool,
}

impl AudioStream {
    /// Stereo/surround L/R balance in `-1.0..=1.0` (`-1.0` = full left, `0.0` = center/mono, `+1.0` = full right).
    #[must_use]
    pub fn balance(&self) -> f32 {
        compute_balance_with_map(&self.channels, &self.channel_map)
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
    /// Spatial channel positions corresponding to [`Self::channels`].
    pub channel_map: Vec<AudioChannelPosition>,
    pub db: Option<f32>,
    pub muted: bool,
}

impl AudioCapture {
    /// Stereo/surround L/R balance in `-1.0..=1.0` (`-1.0` = full left, `0.0` = center/mono, `+1.0` = full right).
    #[must_use]
    pub fn balance(&self) -> f32 {
        compute_balance_with_map(&self.channels, &self.channel_map)
    }
}

/// Point-in-time snapshot of the default audio sink, default source (mic), available devices,
/// sound cards/profiles, and per-application playback/capture streams.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioSnapshot {
    pub linear: f32,
    /// Per-channel linear volumes of the active default sink (`[left, right, ...]`).
    pub channels: Vec<f32>,
    /// Spatial speaker positions corresponding to [`Self::channels`].
    pub channel_map: Vec<AudioChannelPosition>,
    pub db: Option<f32>,
    pub muted: bool,
    pub default_sink: Option<AudioDevice>,
    pub devices: Vec<AudioDevice>,
    pub source_linear: f32,
    /// Per-channel linear volumes of the active default source (`[left, right, ...]`).
    pub source_channels: Vec<f32>,
    /// Spatial channel positions corresponding to [`Self::source_channels`].
    pub source_channel_map: Vec<AudioChannelPosition>,
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

    /// Stereo/surround L/R balance of the active default sink in `-1.0..=1.0` (`0.0` when centered or mono).
    #[must_use]
    pub fn balance(&self) -> f32 {
        compute_balance_with_map(&self.channels, &self.channel_map)
    }

    /// Stereo/surround L/R balance of the active default source in `-1.0..=1.0` (`0.0` when centered or mono).
    #[must_use]
    pub fn source_balance(&self) -> f32 {
        compute_balance_with_map(&self.source_channels, &self.source_channel_map)
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
    let count = channels.len();
    if count < 2 {
        return 0.0;
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
        return 0.0;
    }
    let l = left_sum / (left_n as f32);
    let r = right_sum / (right_n as f32);
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
        AudioCapture, AudioCard, AudioChannelPosition, AudioDevice, AudioPort,
        AudioPortAvailability, AudioProfile, AudioSnapshot, AudioStream, apply_linear_balance,
        apply_linear_balance_with_map, compute_balance_with_map, compute_stereo_balance,
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
                channel_map: Vec::new(),
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
                channel_map: Vec::new(),
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
    fn surround_5_1_and_7_1_balance_preserves_center_lfe_and_front_rear_fade() {
        // 5.1 layout: [FL, FR, FC, LFE, RL, RR] with custom center (0.7), subwoofer (0.5),
        // and front (0.8) vs rear (0.6) fade.
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

        // Pan 50% left (-0.5):
        // - FL (0.8) and RL (0.6) stay at 0.8 and 0.6 (front/rear fade ratio preserved)
        // - FR (0.8 -> 0.4) and RR (0.6 -> 0.3) scale down by 0.5 (front/rear fade preserved)
        // - FC (0.7) and LFE (0.5) are completely untouched!
        let panned = apply_linear_balance_with_map(&ch_5_1, &map_5_1, 6, 0.8, -0.5);
        assert!((panned[0] - 0.8).abs() < 1e-5); // FL
        assert!((panned[1] - 0.4).abs() < 1e-5); // FR
        assert!((panned[2] - 0.7).abs() < 1e-5); // FC untouched
        assert!((panned[3] - 0.5).abs() < 1e-5); // LFE untouched
        assert!((panned[4] - 0.6).abs() < 1e-5); // RL
        assert!((panned[5] - 0.3).abs() < 1e-5); // RR
        assert!((compute_balance_with_map(&panned, &map_5_1) - (-0.5)).abs() < 1e-5);

        // 7.1 layout: [FL, FR, FC, LFE, RL, RR, SL, SR]
        let map_7_1 = AudioChannelPosition::default_map_for_count(8);
        let ch_7_1 = [0.8, 0.8, 0.75, 0.4, 0.8, 0.8, 0.8, 0.8];
        let panned_7_1 = apply_linear_balance_with_map(&ch_7_1, &map_7_1, 8, 0.8, 0.5);
        assert!((panned_7_1[0] - 0.4).abs() < 1e-5); // FL
        assert!((panned_7_1[1] - 0.8).abs() < 1e-5); // FR
        assert!((panned_7_1[2] - 0.75).abs() < 1e-5); // FC untouched
        assert!((panned_7_1[3] - 0.4).abs() < 1e-5); // LFE untouched
        assert!((panned_7_1[4] - 0.4).abs() < 1e-5); // RL
        assert!((panned_7_1[5] - 0.8).abs() < 1e-5); // RR
        assert!((panned_7_1[6] - 0.4).abs() < 1e-5); // SL
        assert!((panned_7_1[7] - 0.8).abs() < 1e-5); // SR
        assert!((compute_balance_with_map(&panned_7_1, &map_7_1) - 0.5).abs() < 1e-5);
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
            channel_map: vec![
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontRight,
            ],
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
