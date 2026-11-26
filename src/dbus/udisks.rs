//! UDisks2 storage drive, block device, filesystem mount/unmount, drive eject/power-off, and
//! LUKS encrypted volume integration (`org.freedesktop.UDisks2`).
//!
//! ## Separation of Concerns (Polkit & Privilege)
//! Privileged disk operations are authorized by the system's `udisks2` daemon through the active
//! desktop session's Polkit authentication agent (`org.freedesktop.PolicyKit1`). `waywi` does not
//! bundle a Polkit agent inside the disk module; instead, actions accept standard UDisks2 option
//! flags (including interactive Polkit authorization control) and execute asynchronously on
//! background D-Bus lanes so interactive Polkit prompts or slow device flushes never block the UI
//! thread.

use std::collections::HashMap;
use std::path::PathBuf;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{
    DBUS_SLOW_TIMEOUT, DbusActionLane, DbusActionReply, DbusClient, DbusSharedBus, LaneActionSpec,
    validate_object_path,
};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{
    ManagedObjects, dict_bool, dict_bytes, dict_f64, dict_i32, dict_i64, dict_object_path,
    dict_str, dict_u64, get_managed_objects, unwrap_value,
};

const UDISKS_DEST: &str = "org.freedesktop.UDisks2";
const UDISKS_PATH: &str = "/org/freedesktop/UDisks2";
const UDISKS_DRIVE_IFACE: &str = "org.freedesktop.UDisks2.Drive";
const UDISKS_DRIVE_ATA_IFACE: &str = "org.freedesktop.UDisks2.Drive.Ata";
const UDISKS_BLOCK_IFACE: &str = "org.freedesktop.UDisks2.Block";
const UDISKS_FS_IFACE: &str = "org.freedesktop.UDisks2.Filesystem";
const UDISKS_ENCRYPTED_IFACE: &str = "org.freedesktop.UDisks2.Encrypted";
const UDISKS_PARTITION_IFACE: &str = "org.freedesktop.UDisks2.Partition";
const UDISKS_PARTITION_TABLE_IFACE: &str = "org.freedesktop.UDisks2.PartitionTable";

/// Options for [`UDisksClient::mount`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UDisksMountOptions {
    pub fs_type: Option<String>,
    pub options: Option<String>,
    /// Whether Polkit may prompt interactively if authorization is required (defaults to `true`).
    pub interactive: bool,
}

impl Default for UDisksMountOptions {
    fn default() -> Self {
        Self {
            fs_type: None,
            options: None,
            interactive: true,
        }
    }
}

impl UDisksMountOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn fs_type(mut self, fs_type: impl Into<String>) -> Self {
        self.fs_type = Some(fs_type.into());
        self
    }

    #[must_use]
    pub fn options(mut self, options: impl Into<String>) -> Self {
        self.options = Some(options.into());
        self
    }

    #[must_use]
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }
}

/// Options for [`UDisksClient::format_block`] (`org.freedesktop.UDisks2.Block.Format`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UDisksFormatOptions {
    /// Filesystem type (e.g. `"ext4"`, `"vfat"`, `"exfat"`, `"btrfs"`, `"empty"`).
    pub fs_type: String,
    /// Optional volume label.
    pub label: Option<String>,
    /// Optional erase mode (`"zero"` or `"ata-secure-erase"`).
    pub erase: Option<String>,
    /// Allow interactive Polkit authorization when needed.
    pub interactive: bool,
}

impl UDisksFormatOptions {
    #[must_use]
    pub fn new(fs_type: impl Into<String>) -> Self {
        Self {
            fs_type: fs_type.into(),
            label: None,
            erase: None,
            interactive: true,
        }
    }

    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    #[must_use]
    pub fn erase(mut self, mode: impl Into<String>) -> Self {
        self.erase = Some(mode.into());
        self
    }

    #[must_use]
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }
}

/// ATA S.M.A.R.T. health telemetry parsed from `org.freedesktop.UDisks2.Drive.Ata`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UDisksSmartHealth {
    pub supported: bool,
    pub enabled: bool,
    pub failing: bool,
    pub failing_valid: bool,
    /// Drive temperature in Kelvin (`SmartTemperature`, `0.0` if unavailable).
    pub temperature_kelvin: f64,
    pub power_on_seconds: u64,
    pub bad_sectors: i64,
    pub failing_attributes_now: i32,
    pub failing_attributes_in_past: i32,
    pub selftest_status: String,
    pub selftest_percent_remaining: i32,
}

impl UDisksSmartHealth {
    /// Converts `temperature_kelvin` to degrees Celsius when valid (`> 0.0`).
    #[must_use]
    pub fn temperature_celsius(&self) -> Option<f64> {
        (self.temperature_kelvin > 0.0).then_some(self.temperature_kelvin - 273.15)
    }
}

/// Physical storage drive descriptor exposed by `org.freedesktop.UDisks2.Drive`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UDisksDrive {
    /// D-Bus object path (e.g. `"/org/freedesktop/UDisks2/drives/SanDisk_Ultra_1234"`).
    pub object_path: String,
    /// Stable UDisks2 drive identifier (`Id`).
    pub id: String,
    /// Drive vendor string (`Vendor`).
    pub vendor: String,
    /// Drive model string (`Model`).
    pub model: String,
    /// Drive serial number (`Serial`).
    pub serial: String,
    /// Total drive capacity in bytes (`Size`).
    pub size_bytes: u64,
    /// Physical interconnect bus (`ConnectionBus`: `"usb"`, `"sdio"`, `"ieee1394"`, or `""`).
    pub connection_bus: String,
    /// Current media type (`Media`, e.g. `"thumb"`, `"flash_sd"`, `"optical_cd"`).
    pub media: String,
    /// `true` if the drive or its media is removable (`Removable`).
    pub removable: bool,
    /// `true` if the media itself can be removed from the drive (`MediaRemovable`).
    pub media_removable: bool,
    /// `true` if media is currently inserted and present (`MediaAvailable`).
    pub media_available: bool,
    /// `true` if the drive supports hardware/media eject (`Ejectable`).
    pub ejectable: bool,
    /// `true` if the drive can be safely powered down (`CanPowerOff`, typical for USB drives).
    pub can_power_off: bool,
    /// `true` if the drive currently holds optical media (`Optical`).
    pub optical: bool,
    /// Optional ATA S.M.A.R.T. health snapshot (`org.freedesktop.UDisks2.Drive.Ata`), boxed to
    /// keep non-ATA drives compact in memory.
    pub smart: Option<Box<UDisksSmartHealth>>,
}

impl UDisksDrive {
    /// Returns `true` when the drive is attached over USB (`connection_bus == "usb"`).
    #[must_use]
    pub fn is_usb(&self) -> bool {
        self.connection_bus.eq_ignore_ascii_case("usb")
    }

    /// Returns a human-friendly display name combining `vendor` and `model` (falling back to `id`).
    #[must_use]
    pub fn display_name(&self) -> String {
        match (self.vendor.trim(), self.model.trim()) {
            ("", "") => self.id.clone(),
            (v, "") => v.to_owned(),
            ("", m) => m.to_owned(),
            (v, m) => format!("{v} {m}"),
        }
    }
}

/// Block device, partition, filesystem, or LUKS container exposed by
/// `org.freedesktop.UDisks2.Block`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UDisksBlockDevice {
    /// D-Bus object path (e.g. `"/org/freedesktop/UDisks2/block_devices/sdb1"`).
    pub object_path: String,
    /// Kernel device node path decoded from `Device` (`ay`, e.g. `"/dev/sdb1"`).
    pub device: String,
    /// Preferred device path decoded from `PreferredDevice` (`ay`).
    pub preferred_device: String,
    /// Owning [`UDisksDrive`] object path (`Drive`), or `None` when `"/"`.
    pub drive_path: Option<String>,
    /// Backing encrypted block device object path (`CryptoBackingDevice`), or `None` when `"/"`.
    pub crypto_backing_device: Option<String>,
    /// Probed usage class (`IdUsage`, e.g. `"filesystem"`, `"crypto"`, `"other"`).
    pub id_usage: String,
    /// Probed filesystem or container type (`IdType`, e.g. `"ext4"`, `"vfat"`, `"exfat"`,
    /// `"ntfs"`, `"btrfs"`, `"crypto_LUKS"`).
    pub id_type: String,
    /// Filesystem or volume label (`IdLabel`).
    pub id_label: String,
    /// Filesystem or volume UUID (`IdUUID`).
    pub id_uuid: String,
    /// Block device size in bytes (`Size`).
    pub size_bytes: u64,
    /// `true` when the block device is read-only (`ReadOnly`).
    pub read_only: bool,
    /// `true` when UDisks2 marks this device as an internal system device (`HintSystem`).
    pub hint_system: bool,
    /// `true` when UDisks2 advises hiding this device from user interfaces (`HintIgnore`).
    pub hint_ignore: bool,
    /// `true` when UDisks2 recommends automatic handling (`HintAuto`).
    pub hint_auto: bool,
    /// `true` when the object implements `org.freedesktop.UDisks2.Partition`.
    pub is_partition: bool,
    /// `true` when the object implements `org.freedesktop.UDisks2.PartitionTable`.
    pub is_partition_table: bool,
    /// `true` when the object implements `org.freedesktop.UDisks2.Filesystem`.
    pub has_filesystem: bool,
    /// `true` when the object implements `org.freedesktop.UDisks2.Encrypted` or
    /// `id_usage == "crypto"`.
    pub is_encrypted: bool,
    /// Unlocked cleartext block device object path (`Encrypted.CleartextDevice`), if unlocked.
    pub cleartext_device: Option<String>,
    /// Active mount point paths decoded from `Filesystem.MountPoints` (`aay`).
    pub mount_points: Vec<String>,
}

impl UDisksBlockDevice {
    /// Returns `true` if this filesystem is currently mounted at one or more paths.
    #[must_use]
    pub fn is_mounted(&self) -> bool {
        !self.mount_points.is_empty()
    }

    /// Returns `true` if this is an encrypted LUKS container that is currently unlocked.
    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        self.cleartext_device.is_some()
    }

    /// Best-effort display label (`id_label` -> `id_uuid` -> `preferred_device` -> `device`).
    #[must_use]
    pub fn display_label(&self) -> &str {
        if !self.id_label.is_empty() {
            &self.id_label
        } else if !self.id_uuid.is_empty() {
            &self.id_uuid
        } else if !self.preferred_device.is_empty() {
            &self.preferred_device
        } else {
            &self.device
        }
    }
}

/// Complete snapshot of `org.freedesktop.UDisks2` drives and block devices.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UDisksSnapshot {
    pub drives: Vec<UDisksDrive>,
    pub block_devices: Vec<UDisksBlockDevice>,
}

impl UDisksSnapshot {
    /// Looks up a [`UDisksDrive`] by its D-Bus object path.
    #[must_use]
    pub fn drive(&self, drive_path: &str) -> Option<&UDisksDrive> {
        self.drives.iter().find(|d| d.object_path == drive_path)
    }

    /// Resolves the owning [`UDisksDrive`] for `block` (following `crypto_backing_device` for
    /// unlocked cleartext volumes when `drive_path` is set on the backing LUKS block).
    #[must_use]
    pub fn drive_for_block(&self, block: &UDisksBlockDevice) -> Option<&UDisksDrive> {
        if let Some(ref dp) = block.drive_path
            && let Some(d) = self.drive(dp)
        {
            return Some(d);
        }
        if let Some(ref backing_path) = block.crypto_backing_device
            && let Some(backing) = self
                .block_devices
                .iter()
                .find(|b| b.object_path == *backing_path)
            && let Some(ref dp) = backing.drive_path
        {
            return self.drive(dp);
        }
        None
    }

    /// Returns user-facing removable or external volumes (mountable filesystems and encrypted LUKS
    /// containers that are not marked `hint_ignore`).
    #[must_use]
    pub fn removable_volumes(&self) -> Vec<&UDisksBlockDevice> {
        self.block_devices
            .iter()
            .filter(|b| {
                !b.hint_ignore
                    && (b.has_filesystem || b.is_encrypted)
                    && (!b.hint_system
                        || self
                            .drive_for_block(b)
                            .is_some_and(|d| d.removable || d.is_usb()))
            })
            .collect()
    }
}

impl Dbus {
    /// Subscribes to `org.freedesktop.UDisks2` hotplug, mount/unmount, and encryption state
    /// changes.
    pub fn udisks<M: 'static>(map: impl Fn(UDisksSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusUDisks {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Zero-cost domain handle for UDisks2 storage drives, filesystems, and LUKS encryption.
#[derive(Clone, Copy)]
pub struct UDisksClient<'a>(pub(super) &'a DbusClient);

impl UDisksClient<'_> {
    /// Returns the latest cached [`UDisksSnapshot`] from an active [`Dbus::udisks`] subscription
    /// without issuing any D-Bus calls (0 ms).
    #[must_use]
    pub fn cached_snapshot(&self) -> Option<UDisksSnapshot> {
        self.0.cache.udisks.read().ok().and_then(|g| g.clone())
    }

    /// Queries `org.freedesktop.UDisks2` synchronously on the system bus.
    pub fn snapshot_blocking(&self) -> Result<UDisksSnapshot> {
        let conn = self.0.async_connection(DbusBus::System)?;
        futures_lite::future::block_on(async move {
            let objs = get_managed_objects(&conn, UDISKS_DEST, UDISKS_PATH).await?;
            Ok(parse_udisks_objects(&objs))
        })
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Filesystem.Mount` action on `block_path`.
    ///
    /// On completion, [`super::client::DbusActionResult::reply`] carries
    /// [`DbusActionReply::MountPath`].
    pub fn mount(&self, block_path: &str, options: &UDisksMountOptions) -> Result<()> {
        let path = validate_object_path(block_path, "UDisks2 block object path")?;
        let fstype_owned = options
            .fs_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let opts_owned = options
            .options
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let interactive = options.interactive;
        let target = path.clone();

        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Filesystem.Mount",
                target,
            )
            .with_dedup(format!("udisks:mount:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let mut map = build_auth_options(interactive);
                if let Some(ref ft) = fstype_owned {
                    map.insert("fstype", Value::from(ft.as_str()));
                }
                if let Some(ref mo) = opts_owned {
                    map.insert("options", Value::from(mo.as_str()));
                }
                let reply = conn
                    .call_method(
                        Some(UDISKS_DEST),
                        path.as_str(),
                        Some(UDISKS_FS_IFACE),
                        "Mount",
                        &(map,),
                    )
                    .await
                    .map_err(|e| WaywiError::DbusError(format!("Filesystem.Mount failed: {e}")))?;
                let mount_dir: String = reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(format!("Filesystem.Mount reply: {e}")))?;
                Ok(DbusActionReply::MountPath(PathBuf::from(mount_dir)))
            },
        )
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Filesystem.Unmount` action on `block_path`.
    pub fn unmount(&self, block_path: &str, force: bool, interactive: bool) -> Result<()> {
        let path = validate_object_path(block_path, "UDisks2 block object path")?;
        let target = path.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Filesystem.Unmount",
                target,
            )
            .with_dedup(format!("udisks:unmount:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let mut options = build_auth_options(interactive);
                if force {
                    options.insert("force", Value::from(true));
                }
                conn.call_method(
                    Some(UDISKS_DEST),
                    path.as_str(),
                    Some(UDISKS_FS_IFACE),
                    "Unmount",
                    &(options,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Filesystem.Unmount failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Drive.Eject` action on `drive_path`.
    pub fn eject(&self, drive_path: &str, interactive: bool) -> Result<()> {
        let path = validate_object_path(drive_path, "UDisks2 drive object path")?;
        let target = path.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Drive.Eject",
                target,
            )
            .with_dedup(format!("udisks:eject:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let options = build_auth_options(interactive);
                conn.call_method(
                    Some(UDISKS_DEST),
                    path.as_str(),
                    Some(UDISKS_DRIVE_IFACE),
                    "Eject",
                    &(options,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Drive.Eject failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Drive.PowerOff` action on `drive_path`
    /// (safely powering down a removable USB drive after its filesystems have been unmounted).
    pub fn power_off(&self, drive_path: &str, interactive: bool) -> Result<()> {
        let path = validate_object_path(drive_path, "UDisks2 drive object path")?;
        let target = path.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Drive.PowerOff",
                target,
            )
            .with_dedup(format!("udisks:poweroff:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let options = build_auth_options(interactive);
                conn.call_method(
                    Some(UDISKS_DEST),
                    path.as_str(),
                    Some(UDISKS_DRIVE_IFACE),
                    "PowerOff",
                    &(options,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Drive.PowerOff failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Encrypted.Unlock` action on a LUKS
    /// encrypted `block_path`.
    ///
    /// On completion, [`super::client::DbusActionResult::reply`] carries
    /// [`DbusActionReply::ObjectPath`] containing the unlocked cleartext block device object path.
    pub fn unlock(&self, block_path: &str, passphrase: &str, interactive: bool) -> Result<()> {
        let path = validate_object_path(block_path, "UDisks2 encrypted block object path")?;
        let secret = passphrase.to_owned();
        let target = path.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Encrypted.Unlock",
                target,
            )
            .with_dedup(format!("udisks:unlock:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let options = build_auth_options(interactive);
                let reply = conn
                    .call_method(
                        Some(UDISKS_DEST),
                        path.as_str(),
                        Some(UDISKS_ENCRYPTED_IFACE),
                        "Unlock",
                        &(secret.as_str(), options),
                    )
                    .await
                    .map_err(|e| WaywiError::DbusError(format!("Encrypted.Unlock failed: {e}")))?;
                let cleartext: OwnedObjectPath = reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(format!("Encrypted.Unlock reply: {e}")))?;
                Ok(DbusActionReply::ObjectPath(cleartext.to_string()))
            },
        )
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Encrypted.Lock` action on a LUKS
    /// encrypted `block_path`.
    pub fn lock(&self, block_path: &str, interactive: bool) -> Result<()> {
        let path = validate_object_path(block_path, "UDisks2 encrypted block object path")?;
        let target = path.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Encrypted.Lock",
                target,
            )
            .with_dedup(format!("udisks:lock:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let options = build_auth_options(interactive);
                conn.call_method(
                    Some(UDISKS_DEST),
                    path.as_str(),
                    Some(UDISKS_ENCRYPTED_IFACE),
                    "Lock",
                    &(options,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Encrypted.Lock failed: {e}")))?;
                Ok(())
            },
        )
    }

    /// Formats a block device asynchronously via `org.freedesktop.UDisks2.Block.Format`.
    pub fn format_block(&self, block_path: &str, options: &UDisksFormatOptions) -> Result<()> {
        let path = validate_object_path(block_path, "UDisks2 block path")?;
        if options.fs_type.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "format fs_type cannot be empty".into(),
            ));
        }
        let target = path.clone();
        let fs_type = options.fs_type.clone();
        let label = options.label.clone();
        let erase = options.erase.clone();
        let interactive = options.interactive;
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Block.Format",
                target,
            )
            .with_dedup(format!("udisks:format:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let mut opts = build_auth_options(interactive);
                if let Some(ref lbl) = label {
                    opts.insert("label", Value::from(lbl.as_str()));
                }
                if let Some(ref e) = erase {
                    opts.insert("erase", Value::from(e.as_str()));
                }
                conn.call_method(
                    Some(UDISKS_DEST),
                    path.as_str(),
                    Some(UDISKS_BLOCK_IFACE),
                    "Format",
                    &(fs_type.as_str(), opts),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Block.Format failed: {e}")))?;
                Ok(())
            },
        )
    }
}

fn build_auth_options(interactive: bool) -> HashMap<&'static str, Value<'static>> {
    let mut map = HashMap::new();
    if !interactive {
        map.insert("auth.no_user_interaction", Value::from(true));
    }
    map
}

pub(crate) fn spawn_udisks_worker(
    sender: calloop::channel::Sender<UDisksSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-udisks",
        Some(UDISKS_DEST),
        None,
        sender,
        shared,
        |conn| async move {
            let objs = get_managed_objects(&conn, UDISKS_DEST, UDISKS_PATH)
                .await
                .ok()?;
            Some(parse_udisks_objects(&objs))
        },
    )
}

fn parse_smart_health(props: &HashMap<String, OwnedValue>) -> UDisksSmartHealth {
    UDisksSmartHealth {
        supported: dict_bool(props, "SmartSupported").unwrap_or(false),
        enabled: dict_bool(props, "SmartEnabled").unwrap_or(false),
        failing: dict_bool(props, "SmartFailing").unwrap_or(false),
        failing_valid: dict_bool(props, "SmartFailingValid").unwrap_or(false),
        temperature_kelvin: dict_f64(props, "SmartTemperature")
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(0.0),
        power_on_seconds: dict_u64(props, "SmartPowerOnSeconds").unwrap_or(0),
        bad_sectors: dict_i64(props, "SmartNumBadSectors").unwrap_or(-1),
        failing_attributes_now: dict_i32(props, "SmartNumAttributesFailing").unwrap_or(-1),
        failing_attributes_in_past: dict_i32(props, "SmartNumAttributesFailedInThePast")
            .unwrap_or(-1),
        selftest_status: dict_str(props, "SmartSelftestStatus").unwrap_or_default(),
        selftest_percent_remaining: dict_i32(props, "SmartSelftestPercentRemaining").unwrap_or(-1),
    }
}

pub(crate) fn parse_udisks_objects(objects: &ManagedObjects) -> UDisksSnapshot {
    let mut drives = Vec::new();
    let mut block_devices = Vec::new();

    for (path, ifaces) in objects {
        if let Some(d_props) = ifaces.get(UDISKS_DRIVE_IFACE) {
            let smart = ifaces
                .get(UDISKS_DRIVE_ATA_IFACE)
                .map(|p| Box::new(parse_smart_health(p)));
            drives.push(UDisksDrive {
                object_path: path.to_string(),
                id: dict_str(d_props, "Id").unwrap_or_default(),
                vendor: dict_str(d_props, "Vendor").unwrap_or_default(),
                model: dict_str(d_props, "Model").unwrap_or_default(),
                serial: dict_str(d_props, "Serial").unwrap_or_default(),
                size_bytes: dict_u64(d_props, "Size").unwrap_or(0),
                connection_bus: dict_str(d_props, "ConnectionBus").unwrap_or_default(),
                media: dict_str(d_props, "Media").unwrap_or_default(),
                removable: dict_bool(d_props, "Removable").unwrap_or(false),
                media_removable: dict_bool(d_props, "MediaRemovable").unwrap_or(false),
                media_available: dict_bool(d_props, "MediaAvailable").unwrap_or(true),
                ejectable: dict_bool(d_props, "Ejectable").unwrap_or(false),
                can_power_off: dict_bool(d_props, "CanPowerOff").unwrap_or(false),
                optical: dict_bool(d_props, "Optical").unwrap_or(false),
                smart,
            });
        }

        if let Some(b_props) = ifaces.get(UDISKS_BLOCK_IFACE) {
            let fs_props = ifaces.get(UDISKS_FS_IFACE);
            let enc_props = ifaces.get(UDISKS_ENCRYPTED_IFACE);
            let id_usage = dict_str(b_props, "IdUsage").unwrap_or_default();
            let is_encrypted = enc_props.is_some() || id_usage == "crypto";
            let cleartext_device = enc_props.and_then(|ep| dict_object_path(ep, "CleartextDevice"));
            let mount_points = fs_props
                .map(|fp| dict_c_bytestring_vec(fp, "MountPoints"))
                .unwrap_or_default();

            block_devices.push(UDisksBlockDevice {
                object_path: path.to_string(),
                device: dict_c_bytestring(b_props, "Device").unwrap_or_default(),
                preferred_device: dict_c_bytestring(b_props, "PreferredDevice").unwrap_or_default(),
                drive_path: dict_object_path(b_props, "Drive"),
                crypto_backing_device: dict_object_path(b_props, "CryptoBackingDevice"),
                id_usage,
                id_type: dict_str(b_props, "IdType").unwrap_or_default(),
                id_label: dict_str(b_props, "IdLabel").unwrap_or_default(),
                id_uuid: dict_str(b_props, "IdUUID").unwrap_or_default(),
                size_bytes: dict_u64(b_props, "Size").unwrap_or(0),
                read_only: dict_bool(b_props, "ReadOnly").unwrap_or(false),
                hint_system: dict_bool(b_props, "HintSystem").unwrap_or(false),
                hint_ignore: dict_bool(b_props, "HintIgnore").unwrap_or(false),
                hint_auto: dict_bool(b_props, "HintAuto").unwrap_or(false),
                is_partition: ifaces.contains_key(UDISKS_PARTITION_IFACE),
                is_partition_table: ifaces.contains_key(UDISKS_PARTITION_TABLE_IFACE),
                has_filesystem: fs_props.is_some(),
                is_encrypted,
                cleartext_device,
                mount_points,
            });
        }
    }

    drives.sort_by(|a, b| a.object_path.cmp(&b.object_path));
    block_devices.sort_by(|a, b| {
        a.device
            .cmp(&b.device)
            .then(a.object_path.cmp(&b.object_path))
    });
    UDisksSnapshot {
        drives,
        block_devices,
    }
}

/// Decodes a NUL-terminated byte-array property (`ay`, as used by UDisks2 for `Device` and
/// `PreferredDevice`), with a fallback for plain D-Bus strings (`s`).
fn dict_c_bytestring(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    if let Some(bytes) = dict_bytes(props, key) {
        return decode_c_bytes(&bytes);
    }
    dict_str(props, key).filter(|s| !s.is_empty())
}

/// Decodes an array of NUL-terminated byte-arrays (`aay`, as used by UDisks2 for
/// `Filesystem.MountPoints`), with a fallback for `as`.
fn dict_c_bytestring_vec(props: &HashMap<String, OwnedValue>, key: &str) -> Vec<String> {
    let Some(val) = props.get(key) else {
        return Vec::new();
    };
    if let Ok(raw_list) = Vec::<Vec<u8>>::try_from(val.clone()) {
        return raw_list.iter().filter_map(|b| decode_c_bytes(b)).collect();
    }
    let Value::Array(arr) = unwrap_value(val) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr.iter() {
        match unwrap_value(item) {
            Value::Str(s) if !s.is_empty() => out.push(s.to_string()),
            Value::Array(inner) => {
                let bytes: Vec<u8> = inner
                    .iter()
                    .filter_map(|v| match unwrap_value(v) {
                        Value::U8(b) => Some(*b),
                        _ => None,
                    })
                    .collect();
                if let Some(decoded) = decode_c_bytes(&bytes) {
                    out.push(decoded);
                }
            }
            _ => {}
        }
    }
    out
}

fn decode_c_bytes(bytes: &[u8]) -> Option<String> {
    let slice = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
    if slice.is_empty() {
        return None;
    }
    let s = String::from_utf8_lossy(slice);
    (!s.is_empty()).then(|| s.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_options_and_auth_polarity() {
        let def = UDisksMountOptions::default();
        assert!(def.interactive);
        assert!(build_auth_options(true).is_empty());
        assert_eq!(
            build_auth_options(false).get("auth.no_user_interaction"),
            Some(&Value::from(true))
        );

        let custom = UDisksMountOptions::new()
            .fs_type("exfat")
            .options("ro,noexec")
            .interactive(false);
        assert_eq!(custom.fs_type.as_deref(), Some("exfat"));
        assert_eq!(custom.options.as_deref(), Some("ro,noexec"));
        assert!(!custom.interactive);
    }

    #[test]
    fn parses_udisks2_drives_filesystems_byte_arrays_and_luks_volumes() {
        let mut objs: ManagedObjects = HashMap::new();

        // 1. USB Drive `/org/freedesktop/UDisks2/drives/USB_Flash_1`
        let drive_path = "/org/freedesktop/UDisks2/drives/USB_Flash_1";
        let mut drive_ifaces = HashMap::new();
        let mut drive_props = HashMap::new();
        drive_props.insert(
            "Id".into(),
            OwnedValue::try_from(Value::from("USB-Flash-1")).unwrap(),
        );
        drive_props.insert(
            "Vendor".into(),
            OwnedValue::try_from(Value::from("Acme")).unwrap(),
        );
        drive_props.insert(
            "Model".into(),
            OwnedValue::try_from(Value::from("FlashDrive")).unwrap(),
        );
        drive_props.insert(
            "ConnectionBus".into(),
            OwnedValue::try_from(Value::from("usb")).unwrap(),
        );
        drive_props.insert("Removable".into(), OwnedValue::from(true));
        drive_props.insert("Ejectable".into(), OwnedValue::from(true));
        drive_props.insert("CanPowerOff".into(), OwnedValue::from(true));
        drive_props.insert("Size".into(), OwnedValue::from(32 * 1024 * 1024 * 1024u64));
        drive_ifaces.insert(UDISKS_DRIVE_IFACE.to_owned(), drive_props);
        objs.insert(OwnedObjectPath::try_from(drive_path).unwrap(), drive_ifaces);

        // 2. Mounted USB Partition `/org/freedesktop/UDisks2/block_devices/sdb1`
        let mut sdb1_ifaces = HashMap::new();
        let mut sdb1_block = HashMap::new();
        sdb1_block.insert(
            "Device".into(),
            OwnedValue::try_from(Value::from(b"/dev/sdb1\0".to_vec())).unwrap(),
        );
        sdb1_block.insert(
            "PreferredDevice".into(),
            OwnedValue::try_from(Value::from(b"/dev/sdb1\0".to_vec())).unwrap(),
        );
        sdb1_block.insert(
            "Drive".into(),
            OwnedValue::try_from(Value::ObjectPath(drive_path.try_into().unwrap())).unwrap(),
        );
        sdb1_block.insert(
            "IdUsage".into(),
            OwnedValue::try_from(Value::from("filesystem")).unwrap(),
        );
        sdb1_block.insert(
            "IdType".into(),
            OwnedValue::try_from(Value::from("exfat")).unwrap(),
        );
        sdb1_block.insert(
            "IdLabel".into(),
            OwnedValue::try_from(Value::from("WORK_USB")).unwrap(),
        );
        sdb1_block.insert("HintSystem".into(), OwnedValue::from(false));
        sdb1_block.insert("HintIgnore".into(), OwnedValue::from(false));

        let mut sdb1_fs = HashMap::new();
        let mp_list: Vec<Vec<u8>> = vec![b"/run/media/user/WORK_USB\0".to_vec()];
        sdb1_fs.insert(
            "MountPoints".into(),
            OwnedValue::try_from(Value::from(mp_list)).unwrap(),
        );

        sdb1_ifaces.insert(UDISKS_BLOCK_IFACE.to_owned(), sdb1_block);
        sdb1_ifaces.insert(UDISKS_FS_IFACE.to_owned(), sdb1_fs);
        sdb1_ifaces.insert(UDISKS_PARTITION_IFACE.to_owned(), HashMap::new());
        objs.insert(
            OwnedObjectPath::try_from("/org/freedesktop/UDisks2/block_devices/sdb1").unwrap(),
            sdb1_ifaces,
        );

        // 3. Encrypted LUKS Partition `/org/freedesktop/UDisks2/block_devices/sdb2`
        let mut sdb2_ifaces = HashMap::new();
        let mut sdb2_block = HashMap::new();
        sdb2_block.insert(
            "Device".into(),
            OwnedValue::try_from(Value::from(b"/dev/sdb2\0".to_vec())).unwrap(),
        );
        sdb2_block.insert(
            "Drive".into(),
            OwnedValue::try_from(Value::ObjectPath(drive_path.try_into().unwrap())).unwrap(),
        );
        sdb2_block.insert(
            "IdUsage".into(),
            OwnedValue::try_from(Value::from("crypto")).unwrap(),
        );
        sdb2_block.insert(
            "IdType".into(),
            OwnedValue::try_from(Value::from("crypto_LUKS")).unwrap(),
        );
        let mut sdb2_enc = HashMap::new();
        sdb2_enc.insert(
            "CleartextDevice".into(),
            OwnedValue::try_from(Value::ObjectPath(
                "/org/freedesktop/UDisks2/block_devices/dm_2d0"
                    .try_into()
                    .unwrap(),
            ))
            .unwrap(),
        );
        sdb2_ifaces.insert(UDISKS_BLOCK_IFACE.to_owned(), sdb2_block);
        sdb2_ifaces.insert(UDISKS_ENCRYPTED_IFACE.to_owned(), sdb2_enc);
        objs.insert(
            OwnedObjectPath::try_from("/org/freedesktop/UDisks2/block_devices/sdb2").unwrap(),
            sdb2_ifaces,
        );

        let snap = parse_udisks_objects(&objs);
        assert_eq!(snap.drives.len(), 1);
        assert!(snap.drives[0].is_usb());
        assert_eq!(snap.drives[0].display_name(), "Acme FlashDrive");

        assert_eq!(snap.block_devices.len(), 2);
        let vol1 = &snap.block_devices[0];
        assert_eq!(vol1.device, "/dev/sdb1");
        assert_eq!(vol1.display_label(), "WORK_USB");
        assert!(vol1.is_mounted());
        assert_eq!(vol1.mount_points, vec!["/run/media/user/WORK_USB"]);
        assert_eq!(
            snap.drive_for_block(vol1).map(|d| d.id.as_str()),
            Some("USB-Flash-1")
        );

        let vol2 = &snap.block_devices[1];
        assert_eq!(vol2.device, "/dev/sdb2");
        assert!(vol2.is_encrypted);
        assert!(vol2.is_unlocked());
        assert_eq!(
            vol2.cleartext_device.as_deref(),
            Some("/org/freedesktop/UDisks2/block_devices/dm_2d0")
        );

        assert_eq!(snap.removable_volumes().len(), 2);
    }

    #[test]
    fn parses_ata_smart_health_and_celsius_conversion() {
        let mut ata = HashMap::new();
        ata.insert("SmartSupported".into(), OwnedValue::from(true));
        ata.insert("SmartEnabled".into(), OwnedValue::from(true));
        ata.insert("SmartFailing".into(), OwnedValue::from(false));
        ata.insert("SmartFailingValid".into(), OwnedValue::from(true));
        ata.insert("SmartTemperature".into(), OwnedValue::from(310.15f64));
        ata.insert("SmartPowerOnSeconds".into(), OwnedValue::from(3600u64));
        ata.insert("SmartNumBadSectors".into(), OwnedValue::from(0i64));
        let smart = parse_smart_health(&ata);
        assert!(smart.supported && smart.enabled && !smart.failing);
        let c = smart.temperature_celsius().expect("valid celsius");
        assert!((c - 37.0).abs() < 1e-6);
        assert_eq!(smart.power_on_seconds, 3600);
    }
}
