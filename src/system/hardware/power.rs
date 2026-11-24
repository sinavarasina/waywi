//! Linux `/sys/class/power_supply` battery and charger telemetry.

use std::path::Path;

use crate::error::Result;
use crate::system::sysfs::read_sysfs_u64;

/// Charging/discharging status reported by `/sys/class/power_supply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerStatus {
    Charging,
    Discharging,
    Full,
    NotCharging,
    Unknown,
}

/// Battery or power supply state read from `/sys/class/power_supply/*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryInfo {
    pub name: String,
    pub status: PowerStatus,
    pub capacity_percent: Option<u8>,
    pub power_now_uw: Option<u64>,
    pub energy_now_uwh: Option<u64>,
    pub energy_full_uwh: Option<u64>,
}

/// Snapshot of battery devices read from `/sys/class/power_supply`.
pub type BatterySnapshot = Vec<BatteryInfo>;

/// Enumerates battery devices under `/sys/class/power_supply`.
pub fn read_batteries() -> Result<Vec<BatteryInfo>> {
    read_batteries_from(Path::new("/sys/class/power_supply"))
}

pub fn read_batteries_from(base: &Path) -> Result<Vec<BatteryInfo>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut batteries = Vec::new();
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let supply_type = std::fs::read_to_string(dir.join("type")).unwrap_or_default();
        if supply_type.trim() != "Battery" {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let status_str = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
        let status = match status_str.trim() {
            "Charging" => PowerStatus::Charging,
            "Discharging" => PowerStatus::Discharging,
            "Full" => PowerStatus::Full,
            "Not charging" => PowerStatus::NotCharging,
            _ => PowerStatus::Unknown,
        };
        let capacity_percent = read_sysfs_u64(&dir.join("capacity")).map(|v| v.min(100) as u8);
        batteries.push(BatteryInfo {
            name,
            status,
            capacity_percent,
            power_now_uw: read_sysfs_u64(&dir.join("power_now")),
            energy_now_uwh: read_sysfs_u64(&dir.join("energy_now")),
            energy_full_uwh: read_sysfs_u64(&dir.join("energy_full")),
        });
    }
    batteries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(batteries)
}
