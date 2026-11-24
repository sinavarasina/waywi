//! Neutral compositor IPC facade and compositor-specific backends.
//!
//! Neutral helpers (`Compositor`, `CompositorClient`, `CompositorEvent`, `CompositorWorkspace`,
//! `WorkspaceTarget`) live in [`adapter`], while compositor-specific IPC implementations live in
//! backend submodules such as [`hyprland`] and [`sway`].

#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
pub mod adapter;
#[cfg(feature = "hyprland-ipc")]
pub mod hyprland;
#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
pub(crate) mod stream_worker;
#[cfg(feature = "sway-ipc")]
pub mod sway;

#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
pub use adapter::{
    Compositor, CompositorClient, CompositorEvent, CompositorWorkspace, WorkspaceTarget,
};
