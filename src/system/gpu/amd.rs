//! AMD GPU (`amdgpu`) sysfs and hwmon telemetry reader.

use crate::system::sysfs::{read_sysfs_string, read_sysfs_u64};
use crate::system::units::ByteSize;

use super::discovery::{DiscoveredDrmCard, base_snapshot};
use super::types::GpuDeviceSnapshot;

pub(crate) fn read_amd_gpu(card: &DiscoveredDrmCard) -> GpuDeviceSnapshot {
    let (mut snap, hwmon) = base_snapshot(card);
    let dev = &card.device_dir;

    snap.utilization_percent =
        read_sysfs_u64(&dev.join("gpu_busy_percent")).map(|v| (v.min(100)) as f32);
    snap.vram_used = read_sysfs_u64(&dev.join("mem_info_vram_used")).map(ByteSize::from_bytes);
    snap.vram_total = read_sysfs_u64(&dev.join("mem_info_vram_total")).map(ByteSize::from_bytes);
    snap.gtt_used = read_sysfs_u64(&dev.join("mem_info_gtt_used")).map(ByteSize::from_bytes);
    snap.gtt_total = read_sysfs_u64(&dev.join("mem_info_gtt_total")).map(ByteSize::from_bytes);

    snap.core_clock_mhz = hwmon.as_ref().and_then(|h| h.freq_mhz("sclk")).or_else(|| {
        read_sysfs_string(&dev.join("pp_dpm_sclk")).and_then(|s| parse_pp_dpm_active_mhz(&s))
    });

    snap.memory_clock_mhz = hwmon.as_ref().and_then(|h| h.freq_mhz("mclk")).or_else(|| {
        read_sysfs_string(&dev.join("pp_dpm_mclk")).and_then(|s| parse_pp_dpm_active_mhz(&s))
    });

    snap
}

/// Parses the active (`*`) frequency in MHz from `pp_dpm_sclk` / `pp_dpm_mclk` sysfs output.
#[must_use]
pub fn parse_pp_dpm_active_mhz(contents: &str) -> Option<u32> {
    for line in contents.lines() {
        if !line.contains('*') {
            continue;
        }
        for token in line.split_whitespace() {
            let trimmed = token
                .strip_suffix("Mhz")
                .or_else(|| token.strip_suffix("MHz"))
                .or_else(|| token.strip_suffix("mhz"));
            if let Some(num) = trimmed
                && let Ok(mhz) = num.parse::<u32>()
            {
                return Some(mhz);
            }
        }
    }
    None
}
