//! Linux GPU telemetry domain (`amdgpu`, `i915`/`xe`, `nvidia`/`nouveau`).

pub mod amd;
pub(crate) mod discovery;
pub mod intel;
pub mod nvidia;
pub mod types;

use std::path::Path;
use std::time::Instant;

use crate::error::Result;

use discovery::{base_snapshot, discover_drm_cards};
pub use types::{GpuDeviceSnapshot, GpuSnapshot, GpuVendor};

/// Reads snapshots for all discovered Linux DRM GPUs under `/sys/class/drm`.
pub fn read_gpus() -> Result<GpuSnapshot> {
    Ok(read_gpus_from(
        Path::new("/sys/class/drm"),
        Path::new("/proc/driver/nvidia/gpus"),
        Instant::now(),
    ))
}

/// Reads GPU telemetry from custom sysfs/procfs roots (primarily for deterministic testing).
#[must_use]
pub fn read_gpus_from(drm_root: &Path, nvidia_proc_root: &Path, now: Instant) -> GpuSnapshot {
    let cards = discover_drm_cards(drm_root);
    let gpus = cards
        .iter()
        .map(|card| match card.vendor {
            GpuVendor::Amd => amd::read_amd_gpu(card),
            GpuVendor::Intel => intel::read_intel_gpu(card),
            GpuVendor::Nvidia => nvidia::read_nvidia_gpu(card, nvidia_proc_root),
            GpuVendor::Other(_) => base_snapshot(card).0,
        })
        .collect();
    GpuSnapshot {
        gpus,
        timestamp: now,
    }
}
