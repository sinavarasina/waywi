pub mod client;
pub mod stream;
pub mod types;

pub use client::{HyprlandClient, apply_workspace_event};
pub(crate) use stream::HyprlandListenerHandle;
pub use stream::parse_event_line;
pub use types::{
    ClientInfo, Hyprland, HyprlandEvent, HyprlandSockets, MonitorInfo, WorkspaceId, WorkspaceInfo,
    WorkspaceRefJson,
};
