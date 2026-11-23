//! Convenience prelude importing the core WayWi surface, primitive, layout,
//! subscription, and runtime types.

pub use crate::accessibility::AccessibleRole;
pub use crate::app::{App, Application, HeadlessRuntime};
pub use crate::capabilities::{Capabilities, Capability};
#[cfg(feature = "clipboard")]
pub use crate::clipboard::{Clipboard, ClipboardEvent, ClipboardSelection, ClipboardSnapshot};
pub use crate::desktop::{
    DesktopAction, DesktopEntry, DesktopSearchMatch, XdgBaseDirs, XdgIconLookup, XdgUserDirs,
    filter_launcher_entries, resolve_icon_path, scan_desktop_entries, search_desktop_entries,
    search_desktop_entries_with_weights, spawn_detached, spawn_detached_in,
    spawn_detached_with_env, spawn_shell_detached,
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
pub use crate::image::{AssetRetention, Image, ImageAsset, ImageFilter, ImageFit, SourceRect};
pub use crate::input::{
    ElementKey, ImeConfig, ImeContentHint, ImeContentPurpose, ImeEvent, ImePreedit,
    ImeSurroundingText, IntoElementKey, Key, KeyChord, KeyEvent, KeyboardLayout,
    KeyboardLayoutInfo, Modifiers, MouseButton, PointerEvent, ScrollEvent, ScrollState,
    TextInputOutcome, TextInputState, TouchEvent,
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
    BacklightDevice, BatteryInfo, BrightnessSnapshot, ByteSize, CalendarDayCell, CalendarMonth,
    CalendarRow, DataRate, DdcMonitorInfo, FreqUnit, LocalDateTime, NetInterfaceKind,
    NetInterfaceSnapshot, NetLinkState, NetworkSnapshot, PrivacyAccessEntry, PrivacyAccessKind,
    PrivacyAccessSource, PrivacySnapshot, RateUnit, SnapshotState, System, TimeFormat,
    TimeSnapshot, UsbDevice, read_usb_devices, read_usb_devices_from,
};
pub use crate::text::{
    Font, FontStyle, FontWeight, SharedStr, Text, TextAlign, TextBoundsMode, TextCursorMetrics,
    TextLayoutOptions,
};
pub use crate::timer::Timer;

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
pub use crate::audio::{
    Audio, AudioCapture, AudioCard, AudioChannelPosition, AudioClient, AudioDevice, AudioEvent,
    AudioPort, AudioPortAvailability, AudioProfile, AudioSnapshot, AudioStream,
};
#[cfg(any(
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa",
    feature = "audio-playback"
))]
pub use crate::audio::{AudioPlayer, SoundAsset, SoundData, SoundPreset};

#[cfg(feature = "control-ipc")]
pub use crate::control::{Control, ControlRequest, Signal};

#[cfg(feature = "hyprland-ipc")]
pub use crate::hyprland::{
    ClientInfo, CursorPos, Hyprland, HyprlandClient, HyprlandDevices, HyprlandEvent,
    HyprlandKeyboard, HyprlandLayout, HyprlandMouse, HyprlandOption, HyprlandOptionValue,
    HyprlandVersion, MonitorInfo, WorkspaceId, WorkspaceInfo, WorkspaceRuleInfo, WorkspaceType,
};

#[cfg(feature = "sway-ipc")]
pub use crate::sway::{Sway, SwayClient, SwayEvent, SwayOutput, SwayWorkspace, WorkspaceRef};

#[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
pub use crate::compositor::{CompositorClient, CompositorWorkspace, WorkspaceTarget};

#[cfg(feature = "dbus")]
pub use crate::dbus::{
    BluetoothAdapter, BluetoothAgentEvent, BluetoothAgentRequest, BluetoothDevice,
    BluetoothSnapshot, BluetoothTransportFilter, Dbus, DbusBus, DbusClient, DbusSignalEvent,
    DbusValue, DesktopNotification, GeoClueSnapshot, LogindEvent, LogindInhibitor,
    LogindPowerCapabilities, LogindSnapshot, ModemAccessTech, ModemInfo, ModemRegistrationState,
    ModemSnapshot, ModemState, MprisPlaybackStatus, MprisSnapshot, NetworkManagerSnapshot,
    NmAccessPoint, NmActiveConnection, NmConnectivity, NmDeviceInfo, NmDeviceType,
    NmSavedConnection, NmState, NmWifiSecurity, NotificationCapabilities, NotificationCloseReason,
    NotificationEvent, NotificationImageData, NotificationUrgency, PortalAccentColor,
    PortalColorScheme, PortalContrast, PortalSettingsSnapshot, PowerActionSupport,
    PowerProfileHold, PowerProfileKind, PowerProfileSnapshot, ScreenSaverSnapshot, TrayCategory,
    TrayEvent, TrayIconPixmap, TrayItemSnapshot, TrayMenuItem, TrayStatus, TrayToolTip,
    UDisksBlockDevice, UDisksDrive, UDisksSnapshot, UPowerDevice, UPowerDeviceKind,
    UPowerDeviceState, UPowerSnapshot,
};
#[cfg(feature = "global-shortcuts")]
pub use crate::dbus::{
    BoundGlobalShortcut, GlobalShortcutDescriptor, GlobalShortcutEvent, GlobalShortcuts,
};
#[cfg(feature = "dbus")]
pub use crate::dbus::{DbusActionLane, DbusActionResult, MprisEvent, MprisLoopStatus};
