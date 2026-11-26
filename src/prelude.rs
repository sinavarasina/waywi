//! Convenience prelude importing the core WayWi surface, primitive, layout,
//! subscription, and runtime types.

pub use crate::accessibility::AccessibleRole;
pub use crate::app::{App, Application, HeadlessRuntime, MemoryConfig};
pub use crate::capabilities::{Capabilities, Capability};
#[cfg(feature = "clipboard")]
pub use crate::clipboard::{Clipboard, ClipboardEvent, ClipboardSelection, ClipboardSnapshot};
pub use crate::desktop::{
    DesktopAction, DesktopEntry, DesktopSearchMatch, LaunchOptions, SpawnOptions, XdgBaseDirs,
    XdgIconLookup, XdgUserDirs, filter_launcher_entries, resolve_icon_path, scan_desktop_entries,
    search_desktop_entries, search_desktop_entries_with_weights, spawn_detached, spawn_shell,
    spawn_with,
};
pub use crate::display::{Output, OutputId, OutputInfo, OutputTransform};
pub use crate::effect::EffectCtx;
pub use crate::error::{ErrorKind, Result, WaywiError};
pub use crate::geometry::{
    Bounds, BufferPx, BufferRect, Edges, ElementAnchor, Length, LogicalPx, LogicalPxExt,
    LogicalSize, Percent, PercentExt, Point, Position, ScaleFactor, Size, Transform,
};
#[cfg(feature = "idle-notify")]
pub use crate::idle::{Idle, IdleEvent};
pub use crate::image::{
    AssetId, AssetRetention, Image, ImageAsset, ImageFilter, ImageFit, SourceRect,
};
pub use crate::input::{
    ElementKey, GestureEvent, GestureKind, GesturePhase, ImeConfig, ImeContentHint,
    ImeContentPurpose, ImeEvent, ImePreedit, ImeSurroundingText, IntoElementKey, Key, KeyChord,
    KeyEvent, KeyboardLayout, KeyboardLayoutInfo, Modifiers, MouseButton, PointerEvent,
    ScrollEvent, ScrollSource, ScrollState, TabletEvent, TabletId, TabletPadAxisSource,
    TabletPadEvent, TabletPadEventKind, TabletPadId, TabletToolAxes, TabletToolEvent,
    TabletToolEventKind, TabletToolId, TabletToolType, TextInputOutcome, TextInputState,
    TouchEvent, TouchPhase,
};
pub use crate::render::{
    Align, Border, Canvas, Clip, Color, Column, CornerRadius, Element, Group, Painter, PathBuilder,
    Rect, RendererPreference, Row,
};
pub use crate::sprite::{Fps, FpsExt, Frames, PlaybackMode, Sprite, SpriteFrame, SpriteSheet};
pub use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionKey, Subscriptions};
pub use crate::surface::{
    Anchor, BufferRetention, InputMode, IntoSurfaceKey, KeyboardInteractivity, Layer, PopupAnchor,
    PopupConstraintAdjustment, PopupGravity, PopupPositioner, Surface, SurfaceKey, SurfaceKind,
    Surfaces,
};
pub use crate::system::{
    BacklightDevice, BatteryInfo, BatterySnapshot, BrightnessSnapshot, ByteSize, CalendarDayCell,
    CalendarMonth, CalendarRow, DataRate, DdcMonitorInfo, FreqUnit, LocalDateTime,
    NetInterfaceKind, NetInterfaceSnapshot, NetLinkState, NetworkSnapshot, PrivacyAccessEntry,
    PrivacyAccessKind, PrivacyAccessSource, PrivacySnapshot, RateUnit, SnapshotState, System,
    ThermalSnapshot, TimeFormat, TimeSnapshot, UsbDevice,
};
pub use crate::text::{
    Font, FontStyle, FontWeight, SharedStr, Text, TextAlign, TextBoundsMode, TextCursorMetrics,
    TextLayoutOptions,
};
pub use crate::timer::Timer;

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
pub use crate::audio::{
    Audio, AudioCard, AudioChannelPosition, AudioClient, AudioDevice, AudioDirection, AudioEvent,
    AudioPort, AudioPortAvailability, AudioPortTarget, AudioProfile, AudioSnapshot, AudioStream,
    AudioTarget, AudioVolume,
};
#[cfg(any(
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa",
    feature = "audio-playback"
))]
pub use crate::audio::{AudioPlayer, SoundAsset, SoundData, SoundId, SoundPlay, SoundPreset};

#[cfg(feature = "control-ipc")]
pub use crate::control::{Control, ControlRequest, Signal};

#[cfg(feature = "hyprland-ipc")]
pub use crate::compositor::hyprland::{
    ClientInfo, CursorPos, Hyprland, HyprlandClient, HyprlandDevices, HyprlandEvent,
    HyprlandKeyboard, HyprlandLayout, HyprlandMouse, HyprlandOption, HyprlandOptionValue,
    HyprlandVersion, HyprlandWorkspaceRef, KeyboardLayoutSwitch, MonitorInfo, WorkspaceId,
    WorkspaceInfo, WorkspaceRuleInfo, WorkspaceType,
};

#[cfg(feature = "sway-ipc")]
pub use crate::compositor::sway::{Sway, SwayClient, SwayEvent, SwayOutput, SwayWorkspace};

#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
pub use crate::compositor::{
    Compositor, CompositorClient, CompositorEvent, CompositorWorkspace, WorkspaceTarget,
};

#[cfg(feature = "dbus")]
pub use crate::dbus::{
    BluetoothAgentEvent, BluetoothAgentReply, BluetoothAgentRequest, BluetoothClient,
    BluetoothSnapshot, Dbus, DbusActionLane, DbusActionReply, DbusActionResult, DbusBus,
    DbusClient, DbusSignalEvent, DbusValue, DesktopNotification, GeoClueClient, GeoClueSnapshot,
    LogindClient, LogindEvent, LogindSnapshot, ModemClient, ModemSnapshot, MprisClient, MprisEvent,
    MprisLoopStatus, MprisPlaybackStatus, MprisSnapshot, NetworkManagerClient,
    NetworkManagerSnapshot, NotificationCapabilities, NotificationCloseReason, NotificationEvent,
    NotificationUrgency, NotificationsClient, PortalClient, PortalColorScheme,
    PortalSettingsSnapshot, PowerAction, PowerProfileKind, PowerProfileSnapshot,
    PowerProfilesClient, ScreenSaverClient, ScreenSaverSnapshot, TrayClient, TrayEvent,
    TrayItemSnapshot, UDisksClient, UDisksMountOptions, UDisksSnapshot, UPowerClient,
    UPowerSnapshot, WifiConnectOptions,
};
#[cfg(feature = "global-shortcuts")]
pub use crate::dbus::{BoundGlobalShortcut, GlobalShortcutDescriptor, GlobalShortcutEvent};
