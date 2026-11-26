//! Cross-compositor Wayland desktop extension types and subscription factories:
//! - `ext_session_lock_v1` ([`SessionLock`], [`SessionLockEvent`])
//! - `ext_image_copy_capture_v1` / `zwlr_screencopy_manager_v1` ([`ScreencopyTarget`], [`ScreencopyFrame`])
//! - `zwlr_foreign_toplevel_management_v1` / `ext_foreign_toplevel_list_v1` ([`ForeignToplevel`], [`ForeignToplevelSnapshot`])
//! - `ext_workspace_v1` ([`ExtWorkspace`], [`ExtWorkspaceSnapshot`])

use bitflags::bitflags;

use crate::arena::FrameSharedFn;
use crate::display::{Output, OutputId};
use crate::geometry::Bounds;
use crate::render::Color;
use crate::subscription::{Subscription, SubscriptionSpec};

/// Lifecycle events emitted by the Wayland `ext_session_lock_v1` session lock protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionLockEvent {
    /// The compositor has locked all outputs and is displaying WayWi's lock surfaces.
    Locked,
    /// The compositor rejected or terminated the lock session (e.g., another client already holds
    /// the lock). Applications should remove their `Surface::session_lock` surfaces or exit.
    Finished,
}

/// Subscription factory for `ext_session_lock_v1` lock lifecycle events.
pub struct SessionLock;

impl SessionLock {
    /// Subscribes to [`SessionLockEvent`] notifications (`Locked` / `Finished`).
    ///
    /// Declaring one or more [`Surface::session_lock`](crate::surface::Surface::session_lock)
    /// surfaces automatically acquires the compositor lock; removing all lock surfaces triggers
    /// `unlock_and_destroy()`.
    pub fn subscribe<M: 'static>(map: impl Fn(SessionLockEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::SessionLock {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Target area requested via [`EffectCtx::capture_screen`](crate::effect::EffectCtx::capture_screen).
#[derive(Debug, Clone, PartialEq)]
pub enum ScreencopyTarget {
    /// Capture an entire Wayland output.
    Output(Output),
    /// Capture a logical sub-region of a Wayland output.
    Region { output: Output, bounds: Bounds },
}

/// Decoded RGBA8 screen capture frame returned asynchronously by
/// [`EffectCtx::capture_screen`](crate::effect::EffectCtx::capture_screen).
///
/// Pixels are always normalized to top-to-bottom straight RGBA8 (`4` bytes per pixel,
/// `stride == width * 4`) regardless of compositor SHM format (`Xrgb8888`, `Argb8888`,
/// `Xbgr8888`, `Abgr8888`) or `y_invert` flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreencopyFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub rgba: Vec<u8>,
}

impl ScreencopyFrame {
    /// Returns `[r, g, b, a]` at pixel coordinate `(x, y)`, or `None` if out of bounds.
    #[must_use]
    pub fn pixel_rgba(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let off = (y as usize)
            .checked_mul(self.stride as usize)?
            .checked_add((x as usize).checked_mul(4)?)?;
        let slice = self.rgba.get(off..off + 4)?;
        Some([slice[0], slice[1], slice[2], slice[3]])
    }

    /// Returns a [`Color`] at pixel coordinate `(x, y)`, or `None` if out of bounds.
    #[must_use]
    pub fn pixel_color(&self, x: u32, y: u32) -> Option<Color> {
        let [r, g, b, a] = self.pixel_rgba(x, y)?;
        Some(Color::rgba(r, g, b, a))
    }

    /// Borrows the raw top-to-bottom RGBA8 pixel slice.
    #[must_use]
    pub fn as_rgba_bytes(&self) -> &[u8] {
        &self.rgba
    }
}

/// Stable runtime handle ID identifying a foreign toplevel window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ForeignToplevelId(pub u64);

bitflags! {
    /// State flags reported for a foreign toplevel window (`zwlr_foreign_toplevel_management_v1`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct ForeignToplevelState: u8 {
        const ACTIVATED  = 1 << 0;
        const MAXIMIZED  = 1 << 1;
        const MINIMIZED  = 1 << 2;
        const FULLSCREEN = 1 << 3;
    }
}

/// Snapshot metadata for a single foreign toplevel window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignToplevelInfo {
    pub id: ForeignToplevelId,
    /// Compositor-provided unique string identifier (`ext_foreign_toplevel_list_v1`, when present).
    pub identifier: Option<String>,
    pub title: String,
    pub app_id: String,
    pub state: ForeignToplevelState,
    pub outputs: Vec<OutputId>,
    pub parent: Option<ForeignToplevelId>,
    /// `true` when this handle supports control actions (`activate`, `close`, `minimize`, etc.).
    pub can_control: bool,
}

/// Atomic snapshot of all foreign toplevel windows reported by the compositor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForeignToplevelSnapshot {
    pub toplevels: Vec<ForeignToplevelInfo>,
    /// `true` when the compositor backs this list with `zwlr_foreign_toplevel_management_v1`.
    pub can_control: bool,
}

/// Subscription factory for cross-compositor foreign toplevel window tracking
/// (`zwlr_foreign_toplevel_management_v1` preferred, fallback to `ext_foreign_toplevel_list_v1`).
pub struct ForeignToplevel;

impl ForeignToplevel {
    /// Subscribes to atomic [`ForeignToplevelSnapshot`] updates whenever windows open, change
    /// title/app_id/state, or close.
    pub fn subscribe<M: 'static>(
        map: impl Fn(ForeignToplevelSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::ForeignToplevel {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Stable runtime handle ID identifying an `ext_workspace_v1` workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtWorkspaceId(pub u64);

/// Stable runtime handle ID identifying an `ext_workspace_v1` workspace group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtWorkspaceGroupId(pub u64);

bitflags! {
    /// State flags for an `ext_workspace_v1` workspace.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct ExtWorkspaceState: u8 {
        const ACTIVE = 1 << 0;
        const URGENT = 1 << 1;
        const HIDDEN = 1 << 2;
    }
}

bitflags! {
    /// Allowed mutations advertised by the compositor for an `ext_workspace_v1` workspace.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct ExtWorkspaceCapabilities: u8 {
        const ACTIVATE   = 1 << 0;
        const DEACTIVATE = 1 << 1;
        const REMOVE     = 1 << 2;
        const ASSIGN     = 1 << 3;
    }
}

/// Snapshot metadata for one workspace in `ext_workspace_v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtWorkspaceInfo {
    pub id: ExtWorkspaceId,
    /// Optional stable compositor-assigned string ID.
    pub protocol_id: Option<String>,
    pub name: String,
    pub coordinates: Vec<u32>,
    pub state: ExtWorkspaceState,
    pub capabilities: ExtWorkspaceCapabilities,
    pub group: Option<ExtWorkspaceGroupId>,
}

/// Snapshot metadata for one workspace group in `ext_workspace_v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtWorkspaceGroupInfo {
    pub id: ExtWorkspaceGroupId,
    pub can_create_workspace: bool,
    pub outputs: Vec<OutputId>,
    pub workspaces: Vec<ExtWorkspaceId>,
}

/// Atomic snapshot of all groups and workspaces committed on `ext_workspace_manager_v1.done`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtWorkspaceSnapshot {
    pub groups: Vec<ExtWorkspaceGroupInfo>,
    pub workspaces: Vec<ExtWorkspaceInfo>,
}

/// Subscription factory for the cross-compositor `ext_workspace_v1` protocol.
pub struct ExtWorkspace;

impl ExtWorkspace {
    /// Subscribes to atomic [`ExtWorkspaceSnapshot`] updates emitted on `done`.
    pub fn subscribe<M: 'static>(
        map: impl Fn(ExtWorkspaceSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::ExtWorkspace {
                map: FrameSharedFn::new(map),
            },
        }
    }
}
