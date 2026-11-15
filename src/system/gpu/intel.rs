//! Intel GPU (`i915` and `xe`) sysfs frequency and DRM `fdinfo` helpers.

use std::collections::BTreeMap;

use crate::system::sysfs::read_sysfs_u32;

use super::discovery::{DiscoveredDrmCard, base_snapshot};
use super::types::GpuDeviceSnapshot;

pub(crate) fn read_intel_gpu(card: &DiscoveredDrmCard) -> GpuDeviceSnapshot {
    let (mut snap, _hwmon) = base_snapshot(card);

    // i915 exposes `gt_cur_freq_mhz` under `cardN` or `gt/gt0/rps_cur_freq_mhz`, while `xe`
    // exposes `device/tile0/gt0/freq0/cur_freq`.
    snap.core_clock_mhz = read_sysfs_u32(&card.card_dir.join("gt_cur_freq_mhz"))
        .or_else(|| read_sysfs_u32(&card.card_dir.join("gt/gt0/rps_cur_freq_mhz")))
        .or_else(|| read_sysfs_u32(&card.device_dir.join("tile0/gt0/freq0/cur_freq")));

    snap
}

/// Parses per-engine accumulated nanoseconds (`drm-engine-<name>: <ns> ns`) from DRM `fdinfo`.
#[must_use]
pub fn parse_drm_fdinfo_engines(contents: &str) -> BTreeMap<String, u64> {
    let mut engines = BTreeMap::new();
    for line in contents.lines() {
        let Some(rest) = line.strip_prefix("drm-engine-") else {
            continue;
        };
        let Some((engine, val)) = rest.split_once(':') else {
            continue;
        };
        let Some(ns_token) = val.split_whitespace().next() else {
            continue;
        };
        if let Ok(ns) = ns_token.parse::<u64>() {
            *engines.entry(engine.trim().to_owned()).or_insert(0) += ns;
        }
    }
    engines
}
