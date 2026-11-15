//! Linux `/sys/class/drm/card*` discovery and shared hwmon baseline assembly.

use std::path::{Path, PathBuf};

use crate::system::hardware::sensors::{HwmonMetrics, read_hwmon_metrics};
use crate::system::sysfs::{parse_hex_u16, read_sysfs_string};

use super::types::{GpuDeviceSnapshot, GpuVendor};

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredDrmCard {
    pub(crate) index: u32,
    pub(crate) card_name: String,
    pub(crate) card_dir: PathBuf,
    pub(crate) device_dir: PathBuf,
    pub(crate) vendor: GpuVendor,
    pub(crate) pci_vendor_id: Option<u16>,
    pub(crate) pci_device_id: Option<u16>,
    pub(crate) driver: Option<String>,
    pub(crate) hwmon_dir: Option<PathBuf>,
}

pub(crate) fn discover_drm_cards(drm_root: &Path) -> Vec<DiscoveredDrmCard> {
    if !drm_root.exists() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(drm_root) else {
        return Vec::new();
    };
    let mut cards = Vec::new();
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(name) = fname.to_str() else {
            continue;
        };
        let Some(idx_str) = name.strip_prefix("card") else {
            continue;
        };
        if idx_str.is_empty() || !idx_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(index) = idx_str.parse::<u32>() else {
            continue;
        };
        let card_dir = entry.path();
        if !card_dir.is_dir() {
            continue;
        }
        let device_dir = card_dir.join("device");
        if !device_dir.is_dir() {
            continue;
        }
        let pci_vendor_id =
            read_sysfs_string(&device_dir.join("vendor")).and_then(|s| parse_hex_u16(&s));
        let pci_device_id =
            read_sysfs_string(&device_dir.join("device")).and_then(|s| parse_hex_u16(&s));
        let vendor = pci_vendor_id
            .map(GpuVendor::from_pci_id)
            .unwrap_or(GpuVendor::Other(0));
        let driver = std::fs::read_link(device_dir.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        let hwmon_dir = first_hwmon_dir(&device_dir);

        cards.push(DiscoveredDrmCard {
            index,
            card_name: name.to_owned(),
            card_dir,
            device_dir,
            vendor,
            pci_vendor_id,
            pci_device_id,
            driver,
            hwmon_dir,
        });
    }
    cards.sort_by_key(|c| c.index);
    cards
}

pub(crate) fn base_snapshot(card: &DiscoveredDrmCard) -> (GpuDeviceSnapshot, Option<HwmonMetrics>) {
    let hwmon = card.hwmon_dir.as_deref().map(read_hwmon_metrics);
    let (temperature_celsius, hotspot_celsius) = hwmon
        .as_ref()
        .map_or((None, None), HwmonMetrics::primary_and_hotspot_temp);
    let snapshot = GpuDeviceSnapshot {
        card_name: card.card_name.clone(),
        vendor: card.vendor,
        pci_vendor_id: card.pci_vendor_id,
        pci_device_id: card.pci_device_id,
        driver: card.driver.clone(),
        model_name: None,
        utilization_percent: None,
        vram_used: None,
        vram_total: None,
        gtt_used: None,
        gtt_total: None,
        core_clock_mhz: None,
        memory_clock_mhz: None,
        temperature_celsius,
        hotspot_celsius,
        power_watts: hwmon.as_ref().and_then(|h| h.power_watts),
        fan_rpm: hwmon.as_ref().and_then(|h| h.fan_rpm),
    };
    (snapshot, hwmon)
}

fn first_hwmon_dir(device_dir: &Path) -> Option<PathBuf> {
    let hwmon_base = device_dir.join("hwmon");
    let entries = std::fs::read_dir(hwmon_base).ok()?;
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.into_iter().next()
}
