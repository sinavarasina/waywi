//! D-Bus integration domain: core client/signal worker and desktop service modules.
//!
//! Service-specific helpers live in dedicated submodules including [`accounts`], [`bluetooth`],
//! [`fprint`], [`gamemode`], [`global_shortcuts`], [`idle_privacy`], [`logind`], [`modem`],
//! [`mpris`], [`network_manager`], [`notifications`], [`polkit`], [`portal_settings`],
//! [`power_profiles`], [`secrets`], [`sensors`], [`services`], [`systemd`], [`tray`],
//! [`udisks`], [`updates`], and [`upower`].

pub mod accounts;
pub mod bluetooth;
pub mod client;
pub mod fprint;
pub mod gamemode;
#[cfg(feature = "global-shortcuts")]
pub mod global_shortcuts;
pub mod idle_privacy;
pub mod logind;
pub mod modem;
pub mod mpris;
pub mod network_manager;
pub mod notifications;
pub mod polkit;
pub mod portal_settings;
pub mod power_profiles;
pub mod secrets;
pub mod sensors;
pub mod services;
pub mod systemd;
pub mod tray;
pub mod types;
pub mod udisks;
pub mod updates;
pub mod upower;
pub(crate) mod worker;
pub(crate) mod zvariant_helpers;

pub(crate) use accounts::spawn_accounts_worker;
pub use accounts::{AccountType, AccountsClient, AccountsSnapshot, UserAccount};
pub use bluetooth::{
    BluetoothAdapter, BluetoothAgentEvent, BluetoothAgentReply, BluetoothAgentRequest,
    BluetoothClient, BluetoothDevice, BluetoothSnapshot, BluetoothTransportFilter,
};
pub(crate) use bluetooth::{BluetoothAgentWorker, spawn_bluetooth_worker};
pub use client::{DbusActionLane, DbusActionReply, DbusActionResult, DbusClient};
pub(crate) use client::{DbusActionResultWorker, DbusSharedBus};
pub(crate) use fprint::FprintVerifyWorker;
pub use fprint::{FprintClient, FprintDeviceInfo, FprintEvent, FprintVerifyResult};
pub(crate) use gamemode::spawn_gamemode_worker;
pub use gamemode::{GameModeClient, GameModeSnapshot};
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
pub use network_manager::{
    HotspotOptions, NetworkManagerClient, NetworkManagerSnapshot, NmAccessPoint,
    NmActiveConnection, NmConnectivity, NmDeviceInfo, NmDeviceType, NmSavedConnection,
    NmSecretAgentEvent, NmSecretAgentReply, NmSecretAgentRequest, NmState, NmVpnState,
    NmWifiSecurity, WifiConnectOptions, WifiEnterpriseOptions,
};
pub(crate) use network_manager::{NetworkManagerWorker, NmSecretAgentWorker};
pub(crate) use notifications::NotificationServerWorker;
pub use notifications::{
    DesktopNotification, NotificationCapabilities, NotificationCloseReason, NotificationEvent,
    NotificationImageData, NotificationUrgency, NotificationsClient, OutgoingNotification,
};
pub(crate) use polkit::PolkitAgentWorker;
pub use polkit::{
    PolkitAgentEvent, PolkitAgentReply, PolkitAuthRequest, PolkitAuthResult, PolkitClient,
    PolkitIdentity,
};
pub(crate) use portal_settings::spawn_portal_settings_worker;
pub use portal_settings::{
    PortalAccentColor, PortalBackgroundOptions, PortalBackgroundResult, PortalClient,
    PortalColorScheme, PortalContrast, PortalFileChooserOptions, PortalFileFilter,
    PortalInhibitFlags, PortalOpenUriOptions, PortalScreenCastCursorMode, PortalScreenCastOptions,
    PortalScreenCastResult, PortalScreenCastSourceType, PortalScreenCastStream,
    PortalScreenshotOptions, PortalSettingsSnapshot,
};
pub(crate) use power_profiles::PowerProfilesWorker;
pub use power_profiles::{
    PowerProfileHold, PowerProfileKind, PowerProfileSnapshot, PowerProfilesClient,
};
pub use secrets::{SecretItemInfo, SecretsClient};
pub(crate) use sensors::SensorsWorker;
pub use sensors::{LightLevelUnit, SensorOrientation, SensorProxySnapshot, SensorsClient};
pub use services::{
    AvahiClient, AvahiServiceEntry, AvahiSnapshot, ColordClient, ColordDevice, ColordProfile,
    ColordSnapshot, CupsEvent, KdeConnectClient, KdeConnectDevice, KdeConnectSnapshot,
};
pub(crate) use services::{CupsWorker, spawn_colord_worker, spawn_kdeconnect_worker};
pub use systemd::{
    HostnameClient, HostnameSnapshot, LocaleClient, LocaleSnapshot, SystemdClient, SystemdUnitInfo,
    SystemdUnitsSnapshot, TimedateClient, TimedateSnapshot,
};
pub(crate) use systemd::{SystemdUnitsWorker, spawn_timedate_worker};
pub(crate) use tray::TrayWorker;
pub use tray::{
    TrayCategory, TrayClient, TrayEvent, TrayIconPixmap, TrayItemSnapshot, TrayMenuItem,
    TrayStatus, TrayToolTip,
};
pub use types::{Dbus, DbusBus, DbusSignalEvent, DbusValue};
pub(crate) use udisks::spawn_udisks_worker;
pub use udisks::{
    UDisksBlockDevice, UDisksClient, UDisksDrive, UDisksFormatOptions, UDisksMountOptions,
    UDisksSmartHealth, UDisksSnapshot,
};
pub(crate) use updates::spawn_fwupd_worker;
pub use updates::{
    FwupdClient, FwupdDevice, FwupdSnapshot, FwupdUpdate, PackageKitClient, PackageKitSnapshot,
    PackageKitUpdate,
};
pub(crate) use upower::spawn_upower_worker;
pub use upower::{UPowerClient, UPowerDevice, UPowerDeviceKind, UPowerDeviceState, UPowerSnapshot};
pub(crate) use worker::{DbusSignalWorker, ServiceWatchWorker};
