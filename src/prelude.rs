//! Convenience prelude importing the core WayWi surface, primitive, layout,
//! subscription, and runtime types.

pub use crate::accessibility::AccessibleRole;
pub use crate::app::{App, Application, HeadlessRuntime};
pub use crate::capabilities::{Capabilities, Capability};
#[cfg(feature = "clipboard")]
pub use crate::clipboard::{Clipboard, ClipboardEvent, ClipboardSelection, ClipboardSnapshot};
pub use crate::desktop::{
    DesktopEntry, XdgIconLookup, resolve_icon_path, scan_desktop_entries, spawn_detached,
    spawn_shell_detached,
};
pub use crate::display::{Output, OutputId, OutputInfo};
pub use crate::effect::EffectCtx;
pub use crate::error::{Result, WaywiError};
pub use crate::geometry::{
    Bounds, BufferPx, BufferRect, Edges, ElementAnchor, GeoRect, Length, LogicalPx, LogicalPxExt,
    LogicalSize, Percent, PercentExt, Point, Position, ScaleFactor, Size, Transform,
};
#[cfg(feature = "idle-notify")]
pub use crate::idle::{Idle, IdleEvent};
pub use crate::image::{Image, ImageAsset, ImageFit};
pub use crate::input::{
    ElementKey, IntoElementKey, Key, KeyChord, KeyEvent, KeyboardLayout, KeyboardLayoutInfo,
    Modifiers, MouseButton, PointerEvent, ScrollEvent, TouchEvent,
};
pub use crate::render::{
    Align, Border, Canvas, Clip, Color, Column, CornerRadius, Element, Group, Painter, PathBuilder,
    Rect, RendererPreference, Row,
};
pub use crate::sprite::{Fps, FpsExt, Frames, PlaybackMode, Sprite, SpriteSheet};
pub use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionKey, Subscriptions};
pub use crate::surface::{
    Anchor, InputMode, IntoSurfaceKey, KeyboardInteractivity, Layer, Surface, SurfaceKey,
    SurfaceKind, Surfaces,
};
pub use crate::system::{
    BacklightDevice, BatteryInfo, BrightnessSnapshot, ByteSize, CalendarDayCell, CalendarMonth,
    CalendarRow, DataRate, DdcMonitorInfo, FreqUnit, LocalDateTime, NetInterfaceKind,
    NetInterfaceSnapshot, NetLinkState, NetworkSnapshot, PrivacyAccessEntry, PrivacyAccessKind,
    PrivacyAccessSource, PrivacySnapshot, RateUnit, SnapshotState, System, TimeFormat,
    TimeSnapshot,
};
pub use crate::text::{Font, FontStyle, FontWeight, SharedStr, Text, TextAlign};
pub use crate::timer::Timer;

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
pub use crate::audio::{Audio, AudioClient, AudioDevice, AudioEvent, AudioSnapshot};
#[cfg(any(
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa",
    feature = "audio-playback"
))]
pub use crate::audio::{AudioPlayer, SoundAsset, SoundData, SoundPreset};

#[cfg(feature = "control-ipc")]
pub use crate::control::{Control, Signal};

#[cfg(feature = "hyprland-ipc")]
pub use crate::hyprland::{
    ClientInfo, Hyprland, HyprlandClient, HyprlandEvent, MonitorInfo, WorkspaceId, WorkspaceInfo,
};

#[cfg(feature = "sway-ipc")]
pub use crate::sway::{Sway, SwayClient, SwayEvent, SwayOutput, SwayWorkspace, WorkspaceRef};

#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
pub use crate::compositor::{CompositorClient, CompositorWorkspace, WorkspaceTarget};

#[cfg(feature = "dbus")]
pub use crate::dbus::{
    BluetoothAdapter, BluetoothDevice, BluetoothSnapshot, Dbus, DbusBus, DbusClient,
    DbusSignalEvent, DbusValue, DesktopNotification, GeoClueSnapshot, LogindEvent, LogindInhibitor,
    LogindSnapshot, ModemAccessTech, ModemInfo, ModemRegistrationState, ModemSnapshot, ModemState,
    MprisPlaybackStatus, MprisSnapshot, NetworkManagerSnapshot, NmAccessPoint, NmActiveConnection,
    NmConnectivity, NmDeviceInfo, NmDeviceType, NmState, NotificationCloseReason,
    NotificationEvent, NotificationImageData, NotificationUrgency, PowerProfileHold,
    PowerProfileKind, PowerProfileSnapshot, ScreenSaverSnapshot, TrayCategory, TrayEvent,
    TrayIconPixmap, TrayItemSnapshot, TrayMenuItem, TrayStatus, TrayToolTip, UPowerDevice,
    UPowerDeviceKind, UPowerDeviceState, UPowerSnapshot,
};
#[cfg(feature = "dbus")]
pub use crate::dbus::{MprisEvent, MprisLoopStatus};
#[cfg(feature = "global-shortcuts")]
pub use crate::dbus::{
    BoundGlobalShortcut, GlobalShortcutDescriptor, GlobalShortcutEvent, GlobalShortcuts,
};
