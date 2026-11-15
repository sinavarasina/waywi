use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

/// Typed Hyprland workspace identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(pub i32);

impl WorkspaceId {
    #[inline]
    #[must_use]
    pub const fn new(id: i32) -> Self {
        Self(id)
    }

    #[inline]
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl From<i32> for WorkspaceId {
    #[inline]
    fn from(value: i32) -> Self {
        Self(value)
    }
}

/// Workspace metadata returned by Hyprland IPC or maintained incrementally from events.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkspaceInfo {
    pub id: WorkspaceId,
    pub name: String,
    #[serde(default)]
    pub monitor: String,
    #[serde(default)]
    pub windows: u32,
    #[serde(skip_deserializing, default)]
    pub focused: bool,
}

/// Active/open client window metadata from Hyprland IPC.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClientInfo {
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub workspace: WorkspaceRefJson,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct WorkspaceRefJson {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub name: String,
}

/// Monitor metadata from Hyprland IPC.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MonitorInfo {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default = "default_scale")]
    pub scale: f32,
    #[serde(default)]
    pub focused: bool,
    #[serde(rename = "activeWorkspace", default)]
    pub active_workspace: WorkspaceRefJson,
}

fn default_scale() -> f32 {
    1.0
}

#[derive(Debug, Deserialize)]
pub(super) struct ActiveWorkspaceJson {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub monitor: String,
    #[serde(default)]
    pub windows: u32,
}

/// Incremental event emitted by Hyprland's `socket2` event stream.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HyprlandEvent {
    Workspace {
        name: String,
    },
    WorkspaceV2 {
        id: WorkspaceId,
        name: String,
    },
    FocusedMonitor {
        monitor: String,
        workspace_name: String,
    },
    FocusedMonitorV2 {
        monitor: String,
        workspace_id: WorkspaceId,
    },
    CreateWorkspace {
        name: String,
    },
    CreateWorkspaceV2 {
        id: WorkspaceId,
        name: String,
    },
    DestroyWorkspace {
        name: String,
    },
    DestroyWorkspaceV2 {
        id: WorkspaceId,
        name: String,
    },
    MoveWorkspaceV2 {
        id: WorkspaceId,
        name: String,
        monitor: String,
    },
    ActiveWindow {
        class: String,
        title: String,
    },
    ActiveWindowV2 {
        address: String,
    },
    OpenWindow {
        address: String,
        workspace: String,
        class: String,
        title: String,
    },
    CloseWindow {
        address: String,
    },
    MonitorAdded {
        name: String,
    },
    MonitorRemoved {
        name: String,
    },
    Fullscreen(bool),
    Submap(String),
    ActiveLayout {
        keyboard: String,
        layout: String,
    },
    Disconnected,
    Reconnected,
    Unknown {
        name: String,
        data: String,
    },
}

/// Hyprland subscription helper (`Hyprland::subscribe(...)`).
pub struct Hyprland;

impl Hyprland {
    /// Subscribes to Hyprland's persistent `socket2` event stream.
    pub fn subscribe<M: 'static>(map: impl Fn(HyprlandEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::HyprlandEvent {
                key: "hyprland-socket2".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Discovered Hyprland Unix socket paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyprlandSockets {
    pub command_socket: PathBuf,
    pub event_socket: PathBuf,
}

impl HyprlandSockets {
    /// Discovers sockets from `$XDG_RUNTIME_DIR` and `$HYPRLAND_INSTANCE_SIGNATURE`.
    pub fn discover() -> Result<Self> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").map_err(|_| {
            WaywiError::ProviderUnavailable("HYPRLAND_INSTANCE_SIGNATURE is not set".to_owned())
        })?;
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or_else(|| {
                WaywiError::ProviderUnavailable("XDG_RUNTIME_DIR is not set".to_owned())
            })?;
        Ok(Self::from_base(&runtime_dir, &sig))
    }

    #[must_use]
    pub fn from_base(runtime_dir: &Path, instance_sig: &str) -> Self {
        let base = runtime_dir.join("hypr").join(instance_sig);
        Self {
            command_socket: base.join(".socket.sock"),
            event_socket: base.join(".socket2.sock"),
        }
    }
}
