//! # WayWi
//!
//! A compiled Wayland widget toolkit in Rust designed for high-density system
//! utilities, OSDs, panels, and compositor overlays.
//!
//! ## Core Model
//!
//! Applications implement [`Application`] using a Model-View-Update architecture:
//! - [`Application::init`] runs once and loads static resources via [`EffectCtx`].
//! - [`Application::update`] handles typed messages and runs bounded side effects.
//! - [`Application::view`] returns [`Surfaces<Message>`] built into scoped double-buffered frame
//!   arenas.
//! - [`Application::subscriptions`] declares timers, system samplers, audio, and IPC streams.

#![warn(clippy::all)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

// Optional process-wide global allocators. Both are compile-time choices made by the final
// binary (opt-in, non-default; see `docs/integrations.md`), so they replace the system
// allocator for the whole process -- including waywi's dependency graph and the frame arena's
// backing chunk reservations.
#[cfg(all(feature = "allocator-mimalloc", feature = "allocator-jemalloc"))]
compile_error!(
    "features `allocator-mimalloc` and `allocator-jemalloc` are mutually exclusive; enable at most one allocator feature"
);

#[cfg(feature = "allocator-mimalloc")]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(not(feature = "allocator-mimalloc"))]
#[cfg(feature = "allocator-jemalloc")]
#[global_allocator]
static GLOBAL_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

pub(crate) mod arena;
pub(crate) mod asset;
pub(crate) mod reconcile;
pub(crate) mod wayland;

pub mod accessibility;
pub mod app;
#[cfg(any(
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa",
    feature = "audio-playback"
))]
pub mod audio;
pub mod capabilities;
#[cfg(feature = "clipboard")]
pub mod clipboard;
#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
pub mod compositor;
#[cfg(feature = "control-ipc")]
pub mod control;
#[cfg(feature = "dbus")]
pub mod dbus;
pub mod desktop;
pub mod display;
pub mod effect;
pub mod error;
pub mod geometry;
#[cfg(feature = "hyprland-ipc")]
pub use compositor::hyprland;
#[cfg(feature = "idle-notify")]
pub mod idle;
pub mod image;
pub mod input;
pub mod prelude;
pub mod render;
pub mod sprite;
pub mod subscription;
pub mod surface;
#[cfg(feature = "sway-ipc")]
pub use compositor::sway;
pub mod system;
pub mod text;
pub mod timer;

pub use app::{App, Application, HeadlessRuntime};
pub use capabilities::{Capabilities, Capability};
pub use display::{Output, OutputId, OutputInfo};
pub use effect::EffectCtx;
pub use error::{Result, WaywiError};
pub use surface::{Surface, Surfaces};
