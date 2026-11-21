//! UDisks2 storage drive, block device, filesystem mount/unmount, drive eject/power-off, and
//! LUKS encrypted volume integration (`org.freedesktop.UDisks2`).
//!
//! ## Separation of Concerns (Polkit & Privilege)
//! Privileged disk operations are authorized by the system's `udisks2` daemon through the active
//! desktop session's Polkit authentication agent (`org.freedesktop.PolicyKit1`). `waywi` does not
//! bundle a Polkit agent inside the disk module; instead, actions accept standard UDisks2 option
//! flags (including `auth.no_user_interaction`) and execute asynchronously on background D-Bus
//! lanes so interactive Polkit prompts or slow device flushes never block the UI thread.

use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{
    DBUS_SLOW_TIMEOUT, DbusActionLane, DbusActionPayload, DbusClient, DbusSharedBus,
    LaneActionSpec, validate_object_path,
};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{
    ManagedObjects, dict_bool, dict_bytes, dict_object_path, dict_str, dict_u64,
    get_managed_objects_async, get_managed_objects_blocking,
};

const UDISKS_DEST: &str = "org.freedesktop.UDisks2";
const UDISKS_PATH: &str = "/org/freedesktop/UDisks2";
const UDISKS_DRIVE_IFACE: &str = "org.freedesktop.UDisks2.Drive";
const UDISKS_BLOCK_IFACE: &str = "org.freedesktop.UDisks2.Block";
const UDISKS_FS_IFACE: &str = "org.freedesktop.UDisks2.Filesystem";
const UDISKS_ENCRYPTED_IFACE: &str = "org.freedesktop.UDisks2.Encrypted";
const UDISKS_PARTITION_IFACE: &str = "org.freedesktop.UDisks2.Partition";
const UDISKS_PARTITION_TABLE_IFACE: &str = "org.freedesktop.UDisks2.PartitionTable";

/// Physical storage drive descriptor exposed by `org.freedesktop.UDisks2.Drive`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
                key: "dbus:udisks".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Reads the current [`UDisksSnapshot`] from the subscription cache (0 ms) or queries
    /// `org.freedesktop.UDisks2`.
    pub fn udisks_snapshot(&self) -> Result<UDisksSnapshot> {
        if let Ok(guard) = self.cache.udisks.read()
            && let Some(ref snap) = *guard
        {
            return Ok(snap.clone());
        }
        let conn = self.connection(DbusBus::System)?;
        let objs = get_managed_objects_blocking(&conn, UDISKS_DEST, UDISKS_PATH)?;
        Ok(parse_udisks_objects(&objs))
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Filesystem.Mount` action on `block_path`.
    ///
    /// On completion, [`super::client::DbusActionResult::reply_object_path`] (and
    /// [`super::client::DbusActionResult::reply`]) contains the mounted directory path returned by
    /// UDisks2.
    pub fn udisks_mount(&self, block_path: &str) -> Result<()> {
        self.udisks_mount_with_options(block_path, None, &[], false)
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Filesystem.Mount` action with optional
    /// filesystem type, mount option flags, and Polkit `auth.no_user_interaction` control.
    pub fn udisks_mount_with_options(
        &self,
        block_path: &str,
        fs_type: Option<&str>,
        mount_options: &[&str],
        no_user_interaction: bool,
    ) -> Result<()> {
        let path = validate_object_path(block_path, "UDisks2 block object path")?;
        let fstype_owned = fs_type
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let opts_joined = (!mount_options.is_empty()).then(|| mount_options.join(","));
        let target = path.clone();

        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Filesystem.Mount",
                target,
            )
            .with_dedup(format!("udisks:mount:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let mut options = build_auth_options(no_user_interaction);
                if let Some(ref ft) = fstype_owned {
                    options.insert("fstype", Value::from(ft.as_str()));
                }
                if let Some(ref mo) = opts_joined {
                    options.insert("options", Value::from(mo.as_str()));
                }
                let reply = conn
                    .call_method(
                        Some(UDISKS_DEST),
                        path.as_str(),
                        Some(UDISKS_FS_IFACE),
                        "Mount",
                        &(options,),
                    )
                    .await
                    .map_err(|e| WaywiError::DbusError(format!("Filesystem.Mount failed: {e}")))?;
                let mount_dir: String = reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(format!("Filesystem.Mount reply: {e}")))?;
                Ok(DbusActionPayload {
                    reply_object_path: Some(mount_dir.clone()),
                    count: None,
                    reply: vec![super::types::DbusValue::String(mount_dir)],
                })
            },
        )
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Filesystem.Unmount` action on
    /// `block_path`.
    pub fn udisks_unmount(&self, block_path: &str, force: bool) -> Result<()> {
        self.udisks_unmount_with_options(block_path, force, false)
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Filesystem.Unmount` action with explicit
    /// `force` and Polkit `auth.no_user_interaction` flags.
    pub fn udisks_unmount_with_options(
        &self,
        block_path: &str,
        force: bool,
        no_user_interaction: bool,
    ) -> Result<()> {
        let path = validate_object_path(block_path, "UDisks2 block object path")?;
        let target = path.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Filesystem.Unmount",
                target,
            )
            .with_dedup(format!("udisks:unmount:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let mut options = build_auth_options(no_user_interaction);
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
    pub fn udisks_eject_drive(&self, drive_path: &str) -> Result<()> {
        self.udisks_eject_drive_with_options(drive_path, false)
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Drive.Eject` action with explicit Polkit
    /// `auth.no_user_interaction` control.
    pub fn udisks_eject_drive_with_options(
        &self,
        drive_path: &str,
        no_user_interaction: bool,
    ) -> Result<()> {
        let path = validate_object_path(drive_path, "UDisks2 drive object path")?;
        let target = path.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Drive.Eject",
                target,
            )
            .with_dedup(format!("udisks:eject:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let options = build_auth_options(no_user_interaction);
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
    pub fn udisks_power_off_drive(&self, drive_path: &str) -> Result<()> {
        self.udisks_power_off_drive_with_options(drive_path, false)
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Drive.PowerOff` action with explicit
    /// Polkit `auth.no_user_interaction` control.
    pub fn udisks_power_off_drive_with_options(
        &self,
        drive_path: &str,
        no_user_interaction: bool,
    ) -> Result<()> {
        let path = validate_object_path(drive_path, "UDisks2 drive object path")?;
        let target = path.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Drive.PowerOff",
                target,
            )
            .with_dedup(format!("udisks:poweroff:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let options = build_auth_options(no_user_interaction);
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
    /// On completion, [`super::client::DbusActionResult::reply_object_path`] contains the unlocked
    /// cleartext block device object path (e.g. `"/org/freedesktop/UDisks2/block_devices/dm_2d0"`).
    pub fn udisks_unlock_encrypted(&self, block_path: &str, passphrase: &str) -> Result<()> {
        self.udisks_unlock_encrypted_with_options(block_path, passphrase, false)
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Encrypted.Unlock` action with explicit
    /// Polkit `auth.no_user_interaction` control.
    pub fn udisks_unlock_encrypted_with_options(
        &self,
        block_path: &str,
        passphrase: &str,
        no_user_interaction: bool,
    ) -> Result<()> {
        let path = validate_object_path(block_path, "UDisks2 encrypted block object path")?;
        let secret = passphrase.to_owned();
        let target = path.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Encrypted.Unlock",
                target,
            )
            .with_dedup(format!("udisks:unlock:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let options = build_auth_options(no_user_interaction);
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
                let cleartext_str = cleartext.to_string();
                Ok(DbusActionPayload {
                    reply_object_path: Some(cleartext_str.clone()),
                    count: None,
                    reply: vec![super::types::DbusValue::ObjectPath(cleartext_str)],
                })
            },
        )
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Encrypted.Lock` action on a LUKS
    /// encrypted `block_path`.
    pub fn udisks_lock_encrypted(&self, block_path: &str) -> Result<()> {
        self.udisks_lock_encrypted_with_options(block_path, false)
    }

    /// Enqueues a non-blocking `org.freedesktop.UDisks2.Encrypted.Lock` action with explicit
    /// Polkit `auth.no_user_interaction` control.
    pub fn udisks_lock_encrypted_with_options(
        &self,
        block_path: &str,
        no_user_interaction: bool,
    ) -> Result<()> {
        let path = validate_object_path(block_path, "UDisks2 encrypted block object path")?;
        let target = path.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::System,
                "Encrypted.Lock",
                target,
            )
            .with_dedup(format!("udisks:lock:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let options = build_auth_options(no_user_interaction);
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
}

fn build_auth_options(no_user_interaction: bool) -> HashMap<&'static str, Value<'static>> {
    let mut map = HashMap::new();
    if no_user_interaction {
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
            let objs = get_managed_objects_async(&conn, UDISKS_DEST, UDISKS_PATH).await?;
            Some(parse_udisks_objects(&objs))
        },
    )
}

pub(crate) fn parse_udisks_objects(objects: &ManagedObjects) -> UDisksSnapshot {
    let mut drives = Vec::new();
    let mut block_devices = Vec::new();

    for (path, ifaces) in objects {
        if let Some(d_props) = ifaces.get(UDISKS_DRIVE_IFACE) {
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
    let Value::Array(arr) = &**val else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr.iter() {
        match item {
            Value::Str(s) if !s.is_empty() => out.push(s.to_string()),
            Value::Array(inner) => {
                let bytes: Vec<u8> = inner
                    .iter()
                    .filter_map(|v| match v {
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
}
