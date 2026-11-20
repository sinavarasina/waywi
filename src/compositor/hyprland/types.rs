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

/// Active/open client window metadata from Hyprland IPC (`j/activewindow`, `j/clients`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(from = "ClientInfoJson")]
pub struct ClientInfo {
    /// Hex window address (e.g. `"0x55a8f1234560"`).
    pub address: String,
    /// Current window title.
    pub title: String,
    /// Current window class (`app_id` on Wayland, `WM_CLASS` on XWayland).
    pub class: String,
    /// Initial window class recorded when the window first mapped (`initialClass`).
    pub initial_class: String,
    /// Initial window title recorded when the window first mapped (`initialTitle`).
    pub initial_title: String,
    /// Workspace that currently owns the window.
    pub workspace: WorkspaceRefJson,
    /// Monitor ID owning the window, or `None` when unassigned (`-1`).
    pub monitor: Option<i64>,
    /// Client process ID (`pid`), or `None` when absent or non-positive (`-1` / `0`).
    pub pid: Option<u32>,
    /// Wayland `wp_content_type_v1` content-type hint (`"photo"`, `"video"`, `"game"`, etc.),
    /// normalized to `None` when unset or `"none"`.
    pub content_type: Option<String>,
    /// `true` when the window is in any maximized or fullscreen state (`fullscreen_mode != 0`).
    pub fullscreen: bool,
    /// Raw Hyprland fullscreen mode bitmask (`0` = none, `1` = maximize, `2` = fullscreen,
    /// `3` = both). Legacy boolean payloads map `false` to `0` and `true` to `2`.
    pub fullscreen_mode: u8,
    /// `true` when the window is floating.
    pub floating: bool,
    /// `true` when the window is pinned across workspaces.
    pub pinned: bool,
    /// `true` when the window runs through XWayland.
    pub xwayland: bool,
    /// `true` when the window is currently mapped.
    pub mapped: bool,
    /// `true` when the window is currently hidden (e.g. inactive tab or special workspace).
    pub hidden: bool,
    /// Top-left window position `(x, y)` in global compositor coordinates (`at`).
    pub at: (i32, i32),
    /// Window size `(width, height)` in compositor coordinates (`size`).
    pub size: (i32, i32),
    /// Recency index in Hyprland's focus history (`focusHistoryID`; `0` = most recently focused).
    pub focus_history_id: i32,
    /// `true` when this window is currently inhibiting idle (`inhibitingIdle`).
    pub inhibiting_idle: bool,
    /// Addresses of windows grouped with this window (`grouped`).
    pub grouped: Vec<String>,
    /// Verbatim window tags reported by Hyprland (e.g. from window rules or Lua scripts).
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawFullscreen {
    Bool(bool),
    Mode(u8),
    Signed(i64),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawContentType {
    Str(String),
    Int(i64),
}

#[derive(Debug, Default, Deserialize)]
struct ClientInfoJson {
    #[serde(default)]
    address: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    class: String,
    #[serde(default, rename = "initialClass")]
    initial_class: String,
    #[serde(default, rename = "initialTitle")]
    initial_title: String,
    #[serde(default)]
    workspace: WorkspaceRefJson,
    #[serde(default)]
    monitor: Option<i64>,
    #[serde(default)]
    pid: Option<i64>,
    #[serde(default, rename = "contentType")]
    content_type: Option<RawContentType>,
    #[serde(default)]
    fullscreen: Option<RawFullscreen>,
    #[serde(default)]
    floating: bool,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    xwayland: bool,
    #[serde(default)]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    at: [i32; 2],
    #[serde(default)]
    size: [i32; 2],
    #[serde(default, rename = "focusHistoryID")]
    focus_history_id: i32,
    #[serde(default, rename = "inhibitingIdle")]
    inhibiting_idle: bool,
    #[serde(default)]
    grouped: Option<Vec<String>>,
    #[serde(default)]
    tags: Option<Vec<String>>,
}

impl From<ClientInfoJson> for ClientInfo {
    fn from(raw: ClientInfoJson) -> Self {
        let fullscreen_mode = match raw.fullscreen {
            Some(RawFullscreen::Bool(true)) => 2,
            Some(RawFullscreen::Bool(false)) | None => 0,
            Some(RawFullscreen::Mode(m)) => m,
            Some(RawFullscreen::Signed(v)) => u8::try_from(v.clamp(0, 255)).unwrap_or(0),
        };
        let content_type = match raw.content_type {
            Some(RawContentType::Str(s)) => normalize_content_type(&s),
            Some(RawContentType::Int(1)) => Some("photo".to_owned()),
            Some(RawContentType::Int(2)) => Some("video".to_owned()),
            Some(RawContentType::Int(3)) => Some("game".to_owned()),
            Some(RawContentType::Int(_)) | None => None,
        };
        Self {
            address: raw.address,
            title: raw.title,
            class: raw.class,
            initial_class: raw.initial_class,
            initial_title: raw.initial_title,
            workspace: raw.workspace,
            monitor: raw.monitor.filter(|&m| m >= 0),
            pid: raw
                .pid
                .and_then(|p| (p > 0).then(|| u32::try_from(p).ok()).flatten()),
            content_type,
            fullscreen: fullscreen_mode != 0,
            fullscreen_mode,
            floating: raw.floating,
            pinned: raw.pinned,
            xwayland: raw.xwayland,
            mapped: raw.mapped,
            hidden: raw.hidden,
            at: (raw.at[0], raw.at[1]),
            size: (raw.size[0], raw.size[1]),
            focus_history_id: raw.focus_history_id,
            inhibiting_idle: raw.inhibiting_idle,
            grouped: raw.grouped.unwrap_or_default(),
            tags: raw.tags.unwrap_or_default(),
        }
    }
}

fn normalize_content_type(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        None
    } else if trimmed.eq_ignore_ascii_case("photo") {
        Some("photo".to_owned())
    } else if trimmed.eq_ignore_ascii_case("video") {
        Some("video".to_owned())
    } else if trimmed.eq_ignore_ascii_case("game") {
        Some("game".to_owned())
    } else {
        Some(trimmed.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct WorkspaceRefJson {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub name: String,
}

/// Monitor metadata from Hyprland IPC (`j/monitors`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MonitorInfo {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub make: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(rename = "refreshRate", default)]
    pub refresh_rate: f32,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default = "default_scale")]
    pub scale: f32,
    #[serde(default)]
    pub transform: u8,
    #[serde(default)]
    pub focused: bool,
    #[serde(rename = "dpmsStatus", default = "default_true")]
    pub dpms_status: bool,
    #[serde(default)]
    pub vrr: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(rename = "activeWorkspace", default)]
    pub active_workspace: WorkspaceRefJson,
    #[serde(rename = "specialWorkspace", default)]
    pub special_workspace: WorkspaceRefJson,
}

fn default_scale() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

/// Global compositor cursor position (`j/cursorpos`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct CursorPos {
    pub x: i32,
    pub y: i32,
}

/// Running Hyprland version metadata (`j/version`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct HyprlandVersion {
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub commits: String,
    #[serde(default)]
    pub flags: Vec<String>,
}

/// Keyboard device snapshot from `j/devices`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct HyprlandKeyboard {
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub layout: String,
    #[serde(default)]
    pub variant: String,
    #[serde(default)]
    pub options: String,
    #[serde(default)]
    pub active_layout_index: u32,
    #[serde(default)]
    pub active_keymap: String,
    #[serde(rename = "capsLock", default)]
    pub caps_lock: bool,
    #[serde(rename = "numLock", default)]
    pub num_lock: bool,
    #[serde(default)]
    pub main: bool,
}

/// Pointer/mouse device snapshot from `j/devices`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct HyprlandMouse {
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "defaultSpeed", default)]
    pub default_speed: f32,
    #[serde(rename = "scrollFactor", default)]
    pub scroll_factor: f32,
}

/// Input device inventory returned by `j/devices`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct HyprlandDevices {
    #[serde(default)]
    pub keyboards: Vec<HyprlandKeyboard>,
    #[serde(default)]
    pub mice: Vec<HyprlandMouse>,
}

/// Typed value returned by `j/getoption <name>` across Hyprland versions (including v0.56+
/// `bool`, `css`, `gradient`, and `custom` option shapes).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum HyprlandOptionValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Vec2(f64, f64),
    Custom(String),
}

impl HyprlandOptionValue {
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            Self::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Int(v) => Some(*v != 0),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) | Self::Custom(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Hyprland runtime option descriptor returned by `j/getoption <name>`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(from = "HyprlandOptionJson")]
pub struct HyprlandOption {
    pub option: String,
    pub value: HyprlandOptionValue,
    pub set: bool,
}

#[derive(Debug, Default, Deserialize)]
struct HyprlandOptionJson {
    #[serde(default)]
    option: String,
    #[serde(default)]
    set: bool,
    #[serde(default)]
    int: Option<i64>,
    #[serde(default)]
    float: Option<f64>,
    #[serde(default)]
    bool: Option<bool>,
    #[serde(default)]
    str: Option<String>,
    #[serde(default)]
    vec2: Option<[f64; 2]>,
    #[serde(default)]
    css: Option<String>,
    #[serde(default)]
    gradient: Option<String>,
    #[serde(default)]
    custom: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

impl From<HyprlandOptionJson> for HyprlandOption {
    fn from(raw: HyprlandOptionJson) -> Self {
        let value = if let Some(b) = raw.bool {
            HyprlandOptionValue::Bool(b)
        } else if let Some(i) = raw.int {
            HyprlandOptionValue::Int(i)
        } else if let Some(f) = raw.float {
            HyprlandOptionValue::Float(f)
        } else if let Some(s) = raw.str {
            HyprlandOptionValue::Str(s)
        } else if let Some([x, y]) = raw.vec2 {
            HyprlandOptionValue::Vec2(x, y)
        } else if let Some(c) = raw.css.or(raw.gradient).or(raw.custom).or(raw.color) {
            HyprlandOptionValue::Custom(c)
        } else if let Some((_, extra_val)) = raw.extra.into_iter().next() {
            match extra_val {
                serde_json::Value::Bool(b) => HyprlandOptionValue::Bool(b),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        HyprlandOptionValue::Int(i)
                    } else {
                        HyprlandOptionValue::Float(n.as_f64().unwrap_or(0.0))
                    }
                }
                serde_json::Value::String(s) => HyprlandOptionValue::Custom(s),
                other => HyprlandOptionValue::Custom(other.to_string()),
            }
        } else {
            HyprlandOptionValue::Custom(String::new())
        };
        Self {
            option: raw.option,
            value,
            set: raw.set,
        }
    }
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
    RenameWorkspace {
        id: WorkspaceId,
        name: String,
    },
    ActiveSpecial {
        workspace_name: String,
        monitor: String,
    },
    ActiveSpecialV2 {
        workspace_id: Option<WorkspaceId>,
        workspace_name: String,
        monitor: String,
    },
    ActiveWindow {
        class: String,
        title: String,
    },
    ActiveWindowV2 {
        address: String,
    },
    WindowTitle {
        address: String,
    },
    WindowTitleV2 {
        address: String,
        title: String,
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
    MoveWindowV2 {
        address: String,
        workspace_id: WorkspaceId,
        workspace_name: String,
    },
    ChangeFloatingMode {
        address: String,
        floating: bool,
    },
    Pin {
        address: String,
        pinned: bool,
    },
    Minimized {
        address: String,
        minimized: bool,
    },
    Urgent {
        address: String,
    },
    Bell {
        address: String,
    },
    MonitorAdded {
        name: String,
    },
    MonitorAddedV2 {
        id: i64,
        name: String,
        description: String,
    },
    MonitorRemoved {
        name: String,
    },
    MonitorRemovedV2 {
        id: i64,
        name: String,
        description: String,
    },
    Fullscreen(bool),
    Screencast {
        active: bool,
        owner: u8,
    },
    Submap(String),
    ActiveLayout {
        keyboard: String,
        layout: String,
    },
    ConfigReloaded,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_modern_hyprland_client_info_with_full_metadata() {
        let json = r#"{
            "address": "0x55a8f1234560",
            "mapped": true,
            "workspace": { "id": 2, "name": "2" },
            "floating": false,
            "monitor": 0,
            "class": "org.example.Game",
            "title": "Example Game",
            "initialClass": "org.example.Game",
            "initialTitle": "Example Launcher",
            "pid": 42195,
            "xwayland": true,
            "pinned": false,
            "fullscreen": 2,
            "tags": ["game", "custom*"],
            "contentType": "game"
        }"#;
        let client: ClientInfo = serde_json::from_str(json).expect("modern ClientInfo");
        assert_eq!(client.address, "0x55a8f1234560");
        assert_eq!(client.class, "org.example.Game");
        assert_eq!(client.initial_class, "org.example.Game");
        assert_eq!(client.initial_title, "Example Launcher");
        assert_eq!(client.workspace.id, 2);
        assert_eq!(client.monitor, Some(0));
        assert_eq!(client.pid, Some(42195));
        assert_eq!(client.content_type.as_deref(), Some("game"));
        assert!(client.fullscreen);
        assert_eq!(client.fullscreen_mode, 2);
        assert!(!client.floating);
        assert!(!client.pinned);
        assert!(client.xwayland);
        assert!(client.mapped);
        assert_eq!(client.tags, vec!["game", "custom*"]);
    }

    #[test]
    fn deserializes_legacy_booleans_negative_sentinels_and_none_content_type() {
        let legacy = r#"{
            "address": "0x123",
            "title": "Browser",
            "class": "firefox",
            "monitor": -1,
            "pid": -1,
            "fullscreen": true,
            "contentType": "none",
            "tags": null
        }"#;
        let client: ClientInfo = serde_json::from_str(legacy).expect("legacy ClientInfo");
        assert_eq!(client.monitor, None);
        assert_eq!(client.pid, None);
        assert_eq!(client.content_type, None);
        assert!(client.fullscreen);
        assert_eq!(client.fullscreen_mode, 2);
        assert!(client.tags.is_empty());

        let int_ct = r#"{
            "address": "0x456",
            "title": "Player",
            "class": "mpv",
            "fullscreen": false,
            "contentType": 2
        }"#;
        let media: ClientInfo = serde_json::from_str(int_ct).expect("numeric contentType");
        assert!(!media.fullscreen);
        assert_eq!(media.fullscreen_mode, 0);
        assert_eq!(media.content_type.as_deref(), Some("video"));
    }

    #[test]
    fn deserializes_hyprland_0_56_extended_client_monitor_devices_and_options() {
        let client_json = r#"{
            "address": "0x55ff4f43c180",
            "mapped": true,
            "hidden": false,
            "at": [0, 24],
            "size": [1920, 1056],
            "workspace": { "id": 3, "name": "3" },
            "monitor": 0,
            "class": "org.example.Terminal",
            "title": "Terminal",
            "pid": 1668,
            "grouped": ["0x1", "0x2"],
            "focusHistoryID": 4,
            "inhibitingIdle": true,
            "contentType": "none"
        }"#;
        let client: ClientInfo = serde_json::from_str(client_json).unwrap();
        assert_eq!(client.at, (0, 24));
        assert_eq!(client.size, (1920, 1056));
        assert!(!client.hidden);
        assert_eq!(client.focus_history_id, 4);
        assert!(client.inhibiting_idle);
        assert_eq!(client.grouped, vec!["0x1", "0x2"]);

        let mon_json = r#"{
            "id": 0,
            "name": "DP-1",
            "description": "Acme Display 27",
            "make": "Acme",
            "model": "Display 27",
            "width": 1920,
            "height": 1080,
            "refreshRate": 180.0,
            "x": 0,
            "y": 0,
            "scale": 1,
            "transform": 0,
            "focused": true,
            "dpmsStatus": true,
            "vrr": true,
            "disabled": false,
            "activeWorkspace": { "id": 3, "name": "3" },
            "specialWorkspace": { "id": -98, "name": "special:scratchpad" }
        }"#;
        let mon: MonitorInfo = serde_json::from_str(mon_json).unwrap();
        assert_eq!(mon.refresh_rate, 180.0);
        assert!(mon.dpms_status);
        assert!(mon.vrr);
        assert!(!mon.disabled);
        assert_eq!(mon.special_workspace.name, "special:scratchpad");

        let opt_css: HyprlandOption =
            serde_json::from_str(r#"{"option":"general:gaps_out","css":"1 1 1 1","set":true}"#)
                .unwrap();
        assert_eq!(opt_css.value.as_str(), Some("1 1 1 1"));
        assert!(opt_css.set);

        let opt_grad: HyprlandOption = serde_json::from_str(
            r#"{"option":"general:col.active_border","gradient":"ffcba6f7 ff313244 45deg","set":true}"#,
        )
        .unwrap();
        assert_eq!(opt_grad.value.as_str(), Some("ffcba6f7 ff313244 45deg"));

        let opt_bool: HyprlandOption =
            serde_json::from_str(r#"{"option":"animations:enabled","bool":true,"set":false}"#)
                .unwrap();
        assert_eq!(opt_bool.value.as_bool(), Some(true));
        assert_eq!(opt_bool.value.as_int(), Some(1));

        let opt_int: HyprlandOption =
            serde_json::from_str(r#"{"option":"decoration:rounding","int":8,"set":true}"#).unwrap();
        assert_eq!(opt_int.value.as_int(), Some(8));
        assert_eq!(opt_int.value.as_float(), Some(8.0));

        let opt_float: HyprlandOption =
            serde_json::from_str(r#"{"option":"input:sensitivity","float":0.25,"set":true}"#)
                .unwrap();
        assert_eq!(opt_float.value.as_float(), Some(0.25));

        let opt_str: HyprlandOption =
            serde_json::from_str(r#"{"option":"general:layout","str":"dwindle","set":true}"#)
                .unwrap();
        assert_eq!(opt_str.value.as_str(), Some("dwindle"));

        let opt_vec2: HyprlandOption =
            serde_json::from_str(r#"{"option":"cursor:pos","vec2":[10.5,20.0],"set":true}"#)
                .unwrap();
        assert_eq!(opt_vec2.value, HyprlandOptionValue::Vec2(10.5, 20.0));

        // Hyprland v0.57+ forward-compatible fallback for unknown/table option types.
        let opt_table: HyprlandOption = serde_json::from_str(
            r#"{"option":"decoration:blur:variant","table":{"mode":"static","passes":2},"set":true}"#,
        )
        .unwrap();
        assert!(
            opt_table
                .value
                .as_str()
                .is_some_and(|s| s.contains("\"static\""))
        );
    }
}
