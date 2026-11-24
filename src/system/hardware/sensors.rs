//! Linux `/sys/class/hwmon` thermal, power, fan, and clock sensor readers.

use std::path::Path;

use crate::error::Result;
use crate::system::sysfs::{read_sysfs_i64, read_sysfs_string, read_sysfs_u32, read_sysfs_u64};

/// Normalized temperature reading exposed by `/sys/class/hwmon/hwmon*`.
#[derive(Debug, Clone, PartialEq)]
pub struct ThermalSensor {
    pub hwmon_name: String,
    pub label: String,
    pub celsius: f32,
    pub critical_celsius: Option<f32>,
}

/// Snapshot of thermal sensors read from `/sys/class/hwmon`.
pub type ThermalSnapshot = Vec<ThermalSensor>;

/// Shared per-hwmon telemetry snapshot reused by GPU and hardware sensor readers.
#[derive(Debug, Clone, Default)]
pub(crate) struct HwmonMetrics {
    pub(crate) temps: Vec<(Option<String>, f32)>,
    pub(crate) power_watts: Option<f32>,
    pub(crate) fan_rpm: Option<u32>,
    pub(crate) freqs_mhz: Vec<(Option<String>, u32)>,
}

impl HwmonMetrics {
    pub(crate) fn primary_and_hotspot_temp(&self) -> (Option<f32>, Option<f32>) {
        let mut primary = None;
        let mut hotspot = None;
        for (label, temp) in &self.temps {
            let lower = label.as_deref().unwrap_or_default().to_ascii_lowercase();
            if lower.contains("junction") || lower.contains("hotspot") {
                if hotspot.is_none() {
                    hotspot = Some(*temp);
                }
            } else if primary.is_none()
                || lower.contains("edge")
                || lower.contains("gpu")
                || lower.contains("die")
            {
                primary = Some(*temp);
            }
        }
        (primary, hotspot)
    }

    pub(crate) fn freq_mhz(&self, label_hint: &str) -> Option<u32> {
        self.freqs_mhz.iter().find_map(|(label, mhz)| {
            label
                .as_deref()
                .filter(|l| l.eq_ignore_ascii_case(label_hint))
                .map(|_| *mhz)
        })
    }
}

/// Reads all thermal sensors exposed under `/sys/class/hwmon`.
pub fn read_sensors() -> Result<Vec<ThermalSensor>> {
    read_sensors_from(Path::new("/sys/class/hwmon"))
}

/// Reads thermal sensors from a custom `hwmon` base directory (testable without live sysfs).
pub fn read_sensors_from(base: &Path) -> Result<Vec<ThermalSensor>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let hwmon_name = read_sysfs_string(&dir.join("name"))
            .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
        for (idx, label, temp_c, crit_c) in scan_hwmon_dir(&dir, false).temps {
            out.push(ThermalSensor {
                hwmon_name: hwmon_name.clone(),
                label: label.unwrap_or_else(|| format!("temp{idx}")),
                celsius: temp_c,
                critical_celsius: crit_c,
            });
        }
    }
    out.sort_by(|a, b| a.hwmon_name.cmp(&b.hwmon_name).then(a.label.cmp(&b.label)));
    Ok(out)
}

pub(crate) fn read_hwmon_metrics(hwmon_dir: &Path) -> HwmonMetrics {
    let scan = scan_hwmon_dir(hwmon_dir, true);
    let temps = scan
        .temps
        .into_iter()
        .map(|(_, label, temp_c, _)| (label, temp_c))
        .collect();

    let power_watts = read_sysfs_u64(&hwmon_dir.join("power1_average"))
        .or_else(|| read_sysfs_u64(&hwmon_dir.join("power1_input")))
        .map(|uw| uw as f32 / 1_000_000.0);

    let fan_rpm = read_sysfs_u32(&hwmon_dir.join("fan1_input"));
    let freqs_mhz = scan
        .freqs
        .into_iter()
        .map(|(_, label, mhz)| (label, mhz))
        .collect();

    HwmonMetrics {
        temps,
        power_watts,
        fan_rpm,
        freqs_mhz,
    }
}

struct HwmonScan {
    temps: Vec<(u32, Option<String>, f32, Option<f32>)>,
    freqs: Vec<(u32, Option<String>, u32)>,
}

fn scan_hwmon_dir(hwmon_dir: &Path, read_freqs: bool) -> HwmonScan {
    let mut temps = Vec::new();
    let mut freqs = Vec::new();
    let Ok(entries) = std::fs::read_dir(hwmon_dir) else {
        return HwmonScan { temps, freqs };
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(name) = fname.to_str() else {
            continue;
        };
        if let Some(idx) = name
            .strip_prefix("temp")
            .and_then(|s| s.strip_suffix("_input"))
            .and_then(|s| s.parse::<u32>().ok())
            && let Some(milli) = read_sysfs_i64(&entry.path())
        {
            let label = read_sysfs_string(&hwmon_dir.join(format!("temp{idx}_label")));
            let crit = read_sysfs_i64(&hwmon_dir.join(format!("temp{idx}_crit")))
                .map(|c| c as f32 / 1000.0);
            temps.push((idx, label, milli as f32 / 1000.0, crit));
        } else if read_freqs
            && let Some(idx) = name
                .strip_prefix("freq")
                .and_then(|s| s.strip_suffix("_input"))
                .and_then(|s| s.parse::<u32>().ok())
            && let Some(hz) = read_sysfs_u64(&entry.path())
        {
            let mhz = (hz / 1_000_000) as u32;
            let label = read_sysfs_string(&hwmon_dir.join(format!("freq{idx}_label")));
            freqs.push((idx, label, mhz));
        }
    }
    temps.sort_by_key(|(idx, _, _, _)| *idx);
    freqs.sort_by_key(|(idx, _, _)| *idx);
    HwmonScan { temps, freqs }
}
