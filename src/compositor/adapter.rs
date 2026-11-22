use crate::capabilities::Capability;
use crate::error::{Result, WaywiError};

/// Common neutral workspace descriptor exposed when using [`CompositorClient`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompositorWorkspace {
    pub id: i32,
    pub name: String,
    pub monitor: String,
    pub focused: bool,
    /// `true` when this workspace is a scratchpad/special workspace (e.g. Hyprland `special:*`).
    pub is_special: bool,
    /// Active workspace layout string if reported by the compositor (e.g. `"dwindle"`,
    /// `"master"`, `"monocle"`, `"scrolling"`, `"splith"`, `"tabbed"`).
    pub layout: Option<String>,
}

/// Target workspace identifier for [`CompositorClient::switch_workspace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceTarget {
    Id(i32),
    Name(String),
}

impl From<i32> for WorkspaceTarget {
    fn from(value: i32) -> Self {
        Self::Id(value)
    }
}

impl From<&str> for WorkspaceTarget {
    fn from(value: &str) -> Self {
        Self::Name(value.to_owned())
    }
}

/// Optional common compositor adapter over active Hyprland or Sway IPC backends.
///
/// Operations not supported by the active compositor return honest capability errors while leaving
/// backend-specific [`HyprlandClient`](crate::hyprland::HyprlandClient) /
/// [`SwayClient`](crate::sway::SwayClient) handles directly accessible.
pub struct CompositorClient<'a> {
    #[cfg(feature = "hyprland-ipc")]
    pub(crate) hyprland: Option<&'a crate::hyprland::HyprlandClient>,
    #[cfg(feature = "sway-ipc")]
    pub(crate) sway: Option<&'a crate::sway::SwayClient>,
    pub(crate) _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> CompositorClient<'a> {
    pub fn workspaces(&self) -> Result<Vec<CompositorWorkspace>> {
        #[cfg(feature = "hyprland-ipc")]
        if let Some(h) = self.hyprland {
            return Ok(h
                .workspaces()?
                .into_iter()
                .map(|w| {
                    let is_special = w.is_special();
                    let layout = w.layout.is_known().then(|| w.layout.as_str().to_owned());
                    CompositorWorkspace {
                        id: w.id.0,
                        name: w.name,
                        monitor: w.monitor,
                        focused: w.focused,
                        is_special,
                        layout,
                    }
                })
                .collect());
        }
        #[cfg(feature = "sway-ipc")]
        if let Some(s) = self.sway {
            return Ok(s
                .workspaces()?
                .into_iter()
                .map(|w| CompositorWorkspace {
                    id: w.num,
                    name: w.name,
                    monitor: w.output,
                    focused: w.focused,
                    is_special: false,
                    layout: (!w.layout.is_empty()).then_some(w.layout),
                })
                .collect());
        }
        Err(unsupported_compositor_error())
    }

    pub fn switch_workspace(&self, target: impl Into<WorkspaceTarget>) -> Result<()> {
        let target = target.into();
        #[cfg(feature = "hyprland-ipc")]
        if let Some(h) = self.hyprland {
            return match target {
                WorkspaceTarget::Id(id) => h.switch_workspace(crate::hyprland::WorkspaceId(id)),
                WorkspaceTarget::Name(name) => h.switch_workspace_name(&name),
            };
        }
        #[cfg(feature = "sway-ipc")]
        if let Some(s) = self.sway {
            return match target {
                WorkspaceTarget::Id(id) => {
                    s.switch_workspace(crate::sway::WorkspaceRef::Number(id))
                }
                WorkspaceTarget::Name(ref name) => {
                    s.switch_workspace(crate::sway::WorkspaceRef::Name(name))
                }
            };
        }
        #[cfg(not(any(feature = "hyprland-ipc", feature = "sway-ipc")))]
        let _ = target;
        Err(unsupported_compositor_error())
    }

    /// Advances all keyboards to the next XKB layout on the active compositor.
    pub fn next_keyboard_layout(&self) -> Result<()> {
        #[cfg(feature = "hyprland-ipc")]
        if let Some(h) = self.hyprland {
            return h.next_keyboard_layout();
        }
        #[cfg(feature = "sway-ipc")]
        if let Some(s) = self.sway {
            return s.next_keyboard_layout();
        }
        Err(unsupported_compositor_error())
    }

    /// Switches all keyboards to a specific zero-based XKB layout index on the active compositor.
    pub fn switch_keyboard_layout(&self, index: u32) -> Result<()> {
        #[cfg(feature = "hyprland-ipc")]
        if let Some(h) = self.hyprland {
            return h.switch_keyboard_layout(index);
        }
        #[cfg(feature = "sway-ipc")]
        if let Some(s) = self.sway {
            return s.switch_keyboard_layout(index);
        }
        #[cfg(not(any(feature = "hyprland-ipc", feature = "sway-ipc")))]
        let _ = index;
        Err(unsupported_compositor_error())
    }
}

fn unsupported_compositor_error() -> WaywiError {
    #[cfg(feature = "hyprland-ipc")]
    {
        WaywiError::UnsupportedCapability(Capability::HyprlandIpc)
    }
    #[cfg(all(not(feature = "hyprland-ipc"), feature = "sway-ipc"))]
    {
        WaywiError::UnsupportedCapability(Capability::SwayIpc)
    }
    #[cfg(not(any(feature = "hyprland-ipc", feature = "sway-ipc")))]
    {
        WaywiError::UnsupportedCapability(Capability::HyprlandIpc)
    }
}
