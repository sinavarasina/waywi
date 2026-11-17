use crate::subscription::{Subscription, SubscriptionKey, SubscriptionSpec};

/// Audio node metadata for an output sink or input source device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: u32,
    pub name: String,
    pub description: String,
}

/// Per-application playback stream routed into a sink (PulseAudio sink input, PipeWire
/// `Stream/Output/Audio` node).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioStream {
    pub id: u32,
    pub name: String,
    pub binary: Option<String>,
    pub sink_id: Option<u32>,
    pub linear: f32,
    pub db: Option<f32>,
    pub muted: bool,
}

/// Per-application capture stream recorded from a source (PulseAudio source output, PipeWire
/// `Stream/Input/Audio` node).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioCapture {
    pub id: u32,
    pub name: String,
    pub binary: Option<String>,
    pub source_id: Option<u32>,
    pub linear: f32,
    pub db: Option<f32>,
    pub muted: bool,
}

/// Point-in-time snapshot of the default audio sink, default source (mic), available devices,
/// and per-application playback/capture streams.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioSnapshot {
    pub linear: f32,
    pub db: Option<f32>,
    pub muted: bool,
    pub default_sink: Option<AudioDevice>,
    pub devices: Vec<AudioDevice>,
    pub source_linear: f32,
    pub source_db: Option<f32>,
    pub source_muted: bool,
    pub default_source: Option<AudioDevice>,
    pub sources: Vec<AudioDevice>,
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

/// Event emitted by the audio monitor.
#[non_exhaustive]
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
    use super::{AudioCapture, AudioSnapshot, AudioStream};

    #[test]
    fn percent_helpers_map_unity_to_100_and_unknown_to_zero() {
        let snap = AudioSnapshot {
            linear: 1.5,
            streams: vec![AudioStream {
                id: 3,
                name: "Firefox".into(),
                binary: None,
                sink_id: None,
                linear: 0.5,
                db: None,
                muted: false,
            }],
            captures: vec![AudioCapture {
                id: 5,
                name: "Recorder".into(),
                binary: Some("rec".into()),
                source_id: None,
                linear: 0.75,
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
}
