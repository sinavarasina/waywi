use crate::arena::FrameSharedFn;
use crate::error::Result;
use crate::subscription::{Subscription, SubscriptionSpec};

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

/// Unified workspace switch target shared across [`CompositorClient`],
/// [`HyprlandClient`](crate::compositor::hyprland::HyprlandClient), and
/// [`SwayClient`](crate::compositor::sway::SwayClient).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceTarget<'a> {
    /// Numeric workspace ID or number (e.g. `1`, `2`).
    Id(i32),
    /// Named or special workspace identifier (e.g. `"code"`, `"name:code"`, `"special:magic"`).
    Name(&'a str),
    /// Relative workspace step (`delta > 0` moves forward, `delta < 0` moves backward;
    /// `existing_only` restricts to existing workspaces on Hyprland or the active output on Sway).
    Relative { delta: i32, existing_only: bool },
    /// Switch back to the previously active workspace (`previous` / `back_and_forth`).
    Previous,
}

impl From<i32> for WorkspaceTarget<'_> {
    #[inline]
    fn from(value: i32) -> Self {
        Self::Id(value)
    }
}

impl<'a> From<&'a str> for WorkspaceTarget<'a> {
    #[inline]
    fn from(value: &'a str) -> Self {
        Self::Name(value)
    }
}

impl<'a> From<&'a String> for WorkspaceTarget<'a> {
    #[inline]
    fn from(value: &'a String) -> Self {
        Self::Name(value.as_str())
    }
}

impl<'a> From<&'a CompositorWorkspace> for WorkspaceTarget<'a> {
    #[inline]
    fn from(ws: &'a CompositorWorkspace) -> Self {
        if ws.id > 0 && !ws.is_special {
            Self::Id(ws.id)
        } else if !ws.name.is_empty() {
            Self::Name(&ws.name)
        } else {
            Self::Id(ws.id)
        }
    }
}

#[cfg(feature = "hyprland-ipc")]
impl From<crate::compositor::hyprland::WorkspaceId> for WorkspaceTarget<'_> {
    #[inline]
    fn from(value: crate::compositor::hyprland::WorkspaceId) -> Self {
        Self::Id(value.0)
    }
}

#[cfg(feature = "hyprland-ipc")]
impl<'a> From<&'a crate::compositor::hyprland::WorkspaceInfo> for WorkspaceTarget<'a> {
    #[inline]
    fn from(ws: &'a crate::compositor::hyprland::WorkspaceInfo) -> Self {
        if ws.id.0 <= 0 {
            if !ws.name.is_empty() {
                Self::Name(&ws.name)
            } else if !ws.address.is_empty() {
                Self::Name(&ws.address)
            } else {
                Self::Id(ws.id.0)
            }
        } else {
            Self::Id(ws.id.0)
        }
    }
}

#[cfg(feature = "sway-ipc")]
impl<'a> From<&'a crate::compositor::sway::SwayWorkspace> for WorkspaceTarget<'a> {
    #[inline]
    fn from(ws: &'a crate::compositor::sway::SwayWorkspace) -> Self {
        if ws.num > 0 {
            Self::Id(ws.num)
        } else {
            Self::Name(&ws.name)
        }
    }
}

/// Neutral compositor event emitted by [`Compositor::subscribe`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositorEvent {
    WorkspaceChanged,
    MonitorChanged,
    WindowChanged,
    KeyboardLayoutChanged(String),
    Disconnected,
    Reconnected,
}

impl CompositorEvent {
    /// Maps a [`HyprlandEvent`](crate::compositor::hyprland::HyprlandEvent) into a neutral
    /// [`CompositorEvent`], returning `None` for events without a neutral counterpart.
    #[cfg(feature = "hyprland-ipc")]
    #[must_use]
    pub(crate) fn from_hyprland(ev: &crate::compositor::hyprland::HyprlandEvent) -> Option<Self> {
        use crate::compositor::hyprland::HyprlandEvent;

        match ev {
            HyprlandEvent::Workspace { .. }
            | HyprlandEvent::WorkspaceV2 { .. }
            | HyprlandEvent::CreateWorkspace { .. }
            | HyprlandEvent::CreateWorkspaceV2 { .. }
            | HyprlandEvent::DestroyWorkspace { .. }
            | HyprlandEvent::DestroyWorkspaceV2 { .. }
            | HyprlandEvent::MoveWorkspaceV2 { .. }
            | HyprlandEvent::RenameWorkspace { .. }
            | HyprlandEvent::ActiveSpecial { .. }
            | HyprlandEvent::ActiveSpecialV2 { .. } => Some(Self::WorkspaceChanged),
            HyprlandEvent::FocusedMonitor { .. }
            | HyprlandEvent::FocusedMonitorV2 { .. }
            | HyprlandEvent::MonitorAdded { .. }
            | HyprlandEvent::MonitorAddedV2 { .. }
            | HyprlandEvent::MonitorRemoved { .. }
            | HyprlandEvent::MonitorRemovedV2 { .. } => Some(Self::MonitorChanged),
            HyprlandEvent::ActiveWindow { .. }
            | HyprlandEvent::ActiveWindowV2 { .. }
            | HyprlandEvent::WindowTitle { .. }
            | HyprlandEvent::WindowTitleV2 { .. }
            | HyprlandEvent::OpenWindow { .. }
            | HyprlandEvent::CloseWindow { .. }
            | HyprlandEvent::MoveWindowV2 { .. }
            | HyprlandEvent::ChangeFloatingMode { .. }
            | HyprlandEvent::Pin { .. }
            | HyprlandEvent::Minimized { .. }
            | HyprlandEvent::Urgent { .. }
            | HyprlandEvent::Fullscreen(_) => Some(Self::WindowChanged),
            HyprlandEvent::ActiveLayout { layout, .. } => {
                Some(Self::KeyboardLayoutChanged(layout.clone()))
            }
            HyprlandEvent::Disconnected => Some(Self::Disconnected),
            HyprlandEvent::Reconnected => Some(Self::Reconnected),
            _ => None,
        }
    }

    /// Maps a [`SwayEvent`](crate::compositor::sway::SwayEvent) into a neutral
    /// [`CompositorEvent`], returning `None` for events without a neutral counterpart.
    #[cfg(feature = "sway-ipc")]
    #[must_use]
    pub(crate) fn from_sway(ev: &crate::compositor::sway::SwayEvent) -> Option<Self> {
        use crate::compositor::sway::SwayEvent;

        match ev {
            SwayEvent::Workspace { .. } => Some(Self::WorkspaceChanged),
            SwayEvent::Output { .. } => Some(Self::MonitorChanged),
            SwayEvent::Window { .. } => Some(Self::WindowChanged),
            SwayEvent::Disconnected => Some(Self::Disconnected),
            SwayEvent::Reconnected => Some(Self::Reconnected),
            _ => None,
        }
    }
}

/// Neutral compositor event subscription helper (`Compositor::subscribe(...)`).
pub struct Compositor;

impl Compositor {
    /// Subscribes to neutral workspace, monitor, window, keyboard-layout, and connection events
    /// from whichever compositor IPC backend (Hyprland or Sway) is active.
    pub fn subscribe<M: 'static>(map: impl Fn(CompositorEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::CompositorEvent {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Neutral compositor adapter over the active Hyprland or Sway IPC backend.
///
/// Backend-specific [`HyprlandClient`](crate::compositor::hyprland::HyprlandClient) and
/// [`SwayClient`](crate::compositor::sway::SwayClient) handles remain directly accessible via
/// matching on this enum or calling the backend-specific `EffectCtx` accessors.
#[derive(Debug, Clone, Copy)]
pub enum CompositorClient<'a> {
    #[cfg(feature = "hyprland-ipc")]
    Hyprland(&'a crate::compositor::hyprland::HyprlandClient),
    #[cfg(feature = "sway-ipc")]
    Sway(&'a crate::compositor::sway::SwayClient),
}

impl<'a> CompositorClient<'a> {
    /// Lists all workspaces reported by the active compositor in neutral form.
    pub fn workspaces(&self) -> Result<Vec<CompositorWorkspace>> {
        match *self {
            #[cfg(feature = "hyprland-ipc")]
            Self::Hyprland(h) => Ok(h
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
                .collect()),
            #[cfg(feature = "sway-ipc")]
            Self::Sway(s) => Ok(s
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
                .collect()),
        }
    }

    /// Switches workspace on the active compositor using a unified [`WorkspaceTarget`].
    pub fn switch_workspace<'b>(&self, target: impl Into<WorkspaceTarget<'b>>) -> Result<()> {
        let target = target.into();
        match *self {
            #[cfg(feature = "hyprland-ipc")]
            Self::Hyprland(h) => h.switch_workspace(target),
            #[cfg(feature = "sway-ipc")]
            Self::Sway(s) => s.switch_workspace(target),
        }
    }

    /// Advances all keyboards to the next XKB layout on the active compositor.
    pub fn next_keyboard_layout(&self) -> Result<()> {
        match *self {
            #[cfg(feature = "hyprland-ipc")]
            Self::Hyprland(h) => h.next_keyboard_layout(),
            #[cfg(feature = "sway-ipc")]
            Self::Sway(s) => s.next_keyboard_layout(),
        }
    }

    /// Switches all keyboards to a specific zero-based XKB layout index on the active compositor.
    pub fn switch_keyboard_layout(&self, index: u32) -> Result<()> {
        match *self {
            #[cfg(feature = "hyprland-ipc")]
            Self::Hyprland(h) => h.switch_keyboard_layout(index),
            #[cfg(feature = "sway-ipc")]
            Self::Sway(s) => s.switch_keyboard_layout(index),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_target_conversions_handle_ids_names_and_compositor_types() {
        assert_eq!(WorkspaceTarget::from(3), WorkspaceTarget::Id(3));
        assert_eq!(WorkspaceTarget::from("code"), WorkspaceTarget::Name("code"));
        let owned = "special:magic".to_owned();
        assert_eq!(
            WorkspaceTarget::from(&owned),
            WorkspaceTarget::Name("special:magic")
        );

        let neutral_normal = CompositorWorkspace {
            id: 4,
            name: "4".to_owned(),
            ..CompositorWorkspace::default()
        };
        assert_eq!(
            WorkspaceTarget::from(&neutral_normal),
            WorkspaceTarget::Id(4)
        );
        let neutral_special = CompositorWorkspace {
            id: 0,
            name: "special:term".to_owned(),
            is_special: true,
            ..CompositorWorkspace::default()
        };
        assert_eq!(
            WorkspaceTarget::from(&neutral_special),
            WorkspaceTarget::Name("special:term")
        );

        #[cfg(feature = "hyprland-ipc")]
        {
            use crate::compositor::hyprland::{WorkspaceId, WorkspaceInfo, WorkspaceType};

            assert_eq!(
                WorkspaceTarget::from(WorkspaceId(7)),
                WorkspaceTarget::Id(7)
            );
            let normal = WorkspaceInfo {
                id: WorkspaceId(2),
                name: "2".to_owned(),
                ..WorkspaceInfo::default()
            };
            assert_eq!(WorkspaceTarget::from(&normal), WorkspaceTarget::Id(2));

            // Hyprland v0.57+ named/special workspace has id == 0 -> uses Name.
            let special_v057 = WorkspaceInfo {
                id: WorkspaceId(0),
                address: "special:magic".to_owned(),
                workspace_type: WorkspaceType::Special,
                name: "special:magic".to_owned(),
                ..WorkspaceInfo::default()
            };
            assert_eq!(
                WorkspaceTarget::from(&special_v057),
                WorkspaceTarget::Name("special:magic")
            );

            // Hyprland <0.57 special workspace has negative id -> uses Name.
            let special_legacy = WorkspaceInfo {
                id: WorkspaceId(-98),
                name: "special:scratch".to_owned(),
                ..WorkspaceInfo::default()
            };
            assert_eq!(
                WorkspaceTarget::from(&special_legacy),
                WorkspaceTarget::Name("special:scratch")
            );
        }

        #[cfg(feature = "sway-ipc")]
        {
            use crate::compositor::sway::SwayWorkspace;

            let numbered = SwayWorkspace {
                num: 5,
                name: "5:web".to_owned(),
                ..SwayWorkspace::default()
            };
            assert_eq!(WorkspaceTarget::from(&numbered), WorkspaceTarget::Id(5));

            let named = SwayWorkspace {
                num: -1,
                name: "chat".to_owned(),
                ..SwayWorkspace::default()
            };
            assert_eq!(WorkspaceTarget::from(&named), WorkspaceTarget::Name("chat"));
        }
    }

    #[test]
    fn compositor_event_mapping_from_backends() {
        #[cfg(feature = "hyprland-ipc")]
        {
            use crate::compositor::hyprland::{HyprlandEvent, WorkspaceId};

            assert_eq!(
                CompositorEvent::from_hyprland(&HyprlandEvent::WorkspaceV2 {
                    id: WorkspaceId(2),
                    name: "2".to_owned(),
                }),
                Some(CompositorEvent::WorkspaceChanged)
            );
            assert_eq!(
                CompositorEvent::from_hyprland(&HyprlandEvent::MonitorAdded {
                    name: "DP-1".to_owned(),
                }),
                Some(CompositorEvent::MonitorChanged)
            );
            assert_eq!(
                CompositorEvent::from_hyprland(&HyprlandEvent::ActiveWindow {
                    class: "foot".to_owned(),
                    title: "shell".to_owned(),
                }),
                Some(CompositorEvent::WindowChanged)
            );
            assert_eq!(
                CompositorEvent::from_hyprland(&HyprlandEvent::ActiveLayout {
                    keyboard: "main".to_owned(),
                    layout: "English (US)".to_owned(),
                }),
                Some(CompositorEvent::KeyboardLayoutChanged(
                    "English (US)".to_owned()
                ))
            );
            assert_eq!(
                CompositorEvent::from_hyprland(&HyprlandEvent::Disconnected),
                Some(CompositorEvent::Disconnected)
            );
            assert_eq!(
                CompositorEvent::from_hyprland(&HyprlandEvent::Reconnected),
                Some(CompositorEvent::Reconnected)
            );
            assert_eq!(
                CompositorEvent::from_hyprland(&HyprlandEvent::ConfigReloaded),
                None
            );
        }

        #[cfg(feature = "sway-ipc")]
        {
            use crate::compositor::sway::SwayEvent;

            assert_eq!(
                CompositorEvent::from_sway(&SwayEvent::Workspace {
                    change: "focus".to_owned(),
                    current: None,
                    old: None,
                }),
                Some(CompositorEvent::WorkspaceChanged)
            );
            assert_eq!(
                CompositorEvent::from_sway(&SwayEvent::Output {
                    change: "unspecified".to_owned(),
                }),
                Some(CompositorEvent::MonitorChanged)
            );
            assert_eq!(
                CompositorEvent::from_sway(&SwayEvent::Window {
                    change: "focus".to_owned(),
                    container_id: Some(1),
                    name: Some("term".to_owned()),
                    app_id: Some("foot".to_owned()),
                }),
                Some(CompositorEvent::WindowChanged)
            );
            assert_eq!(
                CompositorEvent::from_sway(&SwayEvent::Disconnected),
                Some(CompositorEvent::Disconnected)
            );
            assert_eq!(
                CompositorEvent::from_sway(&SwayEvent::Reconnected),
                Some(CompositorEvent::Reconnected)
            );
            assert_eq!(
                CompositorEvent::from_sway(&SwayEvent::Mode {
                    change: "resize".to_owned(),
                }),
                None
            );
        }
    }
}
