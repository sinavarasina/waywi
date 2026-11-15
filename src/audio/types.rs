use crate::subscription::{Subscription, SubscriptionKey, SubscriptionSpec};

/// Audio node metadata for an output sink or input source device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: u32,
    pub name: String,
    pub description: String,
}

/// Point-in-time snapshot of the default audio sink, default source (mic), and available devices.
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
