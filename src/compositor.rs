//! Neutral compositor IPC facade and compositor-specific backends.
//!
//! Neutral helpers (`CompositorClient`, `WorkspaceTarget`, `CompositorWorkspace`) live in
//! [`adapter`], while compositor-specific IPC implementations live in backend submodules such
//! as [`hyprland`] and [`sway`]. Re-exports at the crate root (`waywi::hyprland`,
//! `waywi::sway`) preserve backwards compatibility.

#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
pub mod adapter;
#[cfg(feature = "hyprland-ipc")]
pub mod hyprland;
#[cfg(feature = "sway-ipc")]
pub mod sway;

#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
pub use adapter::{CompositorClient, CompositorWorkspace, WorkspaceTarget};
