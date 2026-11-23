//! Audio system domain: backend-agnostic mixer/device control and audio playback.
//!
//! Mixer/device control types live in `types` and `client`, while decoding, procedural
//! presets, XDG Sound Theme lookup, resampling, and playback live in [`sound`] and [`player`].

#[cfg(feature = "alsa")]
pub(crate) mod alsa;
#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
pub(crate) mod backend;
#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
pub mod client;
#[cfg(feature = "pipewire")]
pub(crate) mod pipewire;
pub mod player;
#[cfg(feature = "pulseaudio")]
pub(crate) mod pulse;
pub mod sound;
#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
pub mod types;

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
pub use client::AudioClient;
pub use player::AudioPlayer;
pub use sound::{SoundAsset, SoundData, SoundPreset, default_sound_dirs, resolve_sound_theme_path};
#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
pub use types::{
    Audio, AudioCapture, AudioCard, AudioDevice, AudioEvent, AudioPort, AudioPortAvailability,
    AudioProfile, AudioSnapshot, AudioStream, compute_stereo_balance,
};
