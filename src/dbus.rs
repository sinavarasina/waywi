//! D-Bus integration domain: core client/signal worker and desktop service modules.
//!
//! Service-specific helpers live in dedicated submodules including [`bluetooth`], [`logind`],
//! [`modem`], [`mpris`], [`notifications`], [`tray`], [`power_profiles`], [`network_manager`],
//! [`udisks`], [`upower`], [`idle_privacy`], [`portal_settings`], and [`global_shortcuts`].

pub mod bluetooth;
pub mod client;
#[cfg(feature = "global-shortcuts")]
pub mod global_shortcuts;
pub mod idle_privacy;
pub mod logind;
pub mod modem;
pub mod mpris;
pub mod network_manager;
pub mod notifications;
pub mod portal_settings;
pub mod power_profiles;
pub mod tray;
pub mod types;
pub mod udisks;
pub mod upower;
pub(crate) mod worker;
pub(crate) mod zvariant_helpers;

pub use bluetooth::{
    BluetoothAdapter, BluetoothAgentEvent, BluetoothAgentReply, BluetoothAgentRequest,
    BluetoothClient, BluetoothDevice, BluetoothSnapshot, BluetoothTransportFilter,
};
pub(crate) use bluetooth::{BluetoothAgentWorker, spawn_bluetooth_worker};
pub use client::{DbusActionLane, DbusActionReply, DbusActionResult, DbusClient};
pub(crate) use client::{DbusActionResultWorker, DbusSharedBus};
#[cfg(feature = "global-shortcuts")]
pub(crate) use global_shortcuts::GlobalShortcutsWorker;
#[cfg(feature = "global-shortcuts")]
pub use global_shortcuts::{BoundGlobalShortcut, GlobalShortcutDescriptor, GlobalShortcutEvent};
pub use idle_privacy::{GeoClueClient, GeoClueSnapshot, ScreenSaverClient, ScreenSaverSnapshot};
pub(crate) use idle_privacy::{spawn_geoclue_worker, spawn_screensaver_worker};
pub(crate) use logind::LogindWorker;
pub use logind::{
    LogindClient, LogindEvent, LogindInhibitor, LogindPowerCapabilities, LogindSnapshot,
    PowerAction, PowerActionSupport,
};
pub(crate) use modem::spawn_modem_worker;
pub use modem::{
    ModemAccessTech, ModemClient, ModemInfo, ModemRegistrationState, ModemSnapshot, ModemState,
};
pub(crate) use mpris::MprisWorker;
pub use mpris::{MprisClient, MprisEvent, MprisLoopStatus, MprisPlaybackStatus, MprisSnapshot};
pub(crate) use network_manager::NetworkManagerWorker;
pub use network_manager::{
    NetworkManagerClient, NetworkManagerSnapshot, NmAccessPoint, NmActiveConnection,
    NmConnectivity, NmDeviceInfo, NmDeviceType, NmSavedConnection, NmState, NmWifiSecurity,
    WifiConnectOptions,
};
pub(crate) use notifications::NotificationServerWorker;
pub use notifications::{
    DesktopNotification, NotificationCapabilities, NotificationCloseReason, NotificationEvent,
    NotificationImageData, NotificationUrgency, NotificationsClient,
};
pub(crate) use portal_settings::spawn_portal_settings_worker;
pub use portal_settings::{
    PortalAccentColor, PortalClient, PortalColorScheme, PortalContrast, PortalSettingsSnapshot,
};
pub(crate) use power_profiles::PowerProfilesWorker;
pub use power_profiles::{
    PowerProfileHold, PowerProfileKind, PowerProfileSnapshot, PowerProfilesClient,
};
pub(crate) use tray::TrayWorker;
pub use tray::{
    TrayCategory, TrayClient, TrayEvent, TrayIconPixmap, TrayItemSnapshot, TrayMenuItem,
    TrayStatus, TrayToolTip,
};
pub use types::{Dbus, DbusBus, DbusSignalEvent, DbusValue};
pub(crate) use udisks::spawn_udisks_worker;
pub use udisks::{
    UDisksBlockDevice, UDisksClient, UDisksDrive, UDisksMountOptions, UDisksSnapshot,
};
pub(crate) use upower::spawn_upower_worker;
pub use upower::{UPowerClient, UPowerDevice, UPowerDeviceKind, UPowerDeviceState, UPowerSnapshot};
pub(crate) use worker::{DbusSignalWorker, ServiceWatchWorker};
