//! NVIDIA GPU (`nvidia` / `nouveau`) safe sysfs, hwmon, and procfs metadata reader.
//!
//! Avoids spawning `nvidia-smi` or loading unverified proprietary C FFI ABIs: `nouveau` hwmon
//! temperature/power/fan sensors and `/proc/driver/nvidia/gpus/*/information` model metadata are
//! read directly when accessible, and runtime metrics unavailable to unprivileged sysfs remain `None`.

use std::path::Path;

use super::discovery::{DiscoveredDrmCard, base_snapshot};
use super::types::GpuDeviceSnapshot;

pub(crate) fn read_nvidia_gpu(
    card: &DiscoveredDrmCard,
    nvidia_proc_root: &Path,
) -> GpuDeviceSnapshot {
    let (mut snap, _hwmon) = base_snapshot(card);

    let pci_slot = std::fs::canonicalize(&card.device_dir)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));

    snap.model_name = lookup_nvidia_model(nvidia_proc_root, pci_slot.as_deref());
    snap
}

/// Extracts `Model: ...` from `/proc/driver/nvidia/gpus/<bus>/information`.
#[must_use]
pub fn parse_nvidia_gpu_info(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("Model") {
            let model = val.trim();
            if !model.is_empty() {
                return Some(model.to_owned());
            }
        }
    }
    None
}

fn lookup_nvidia_model(proc_root: &Path, pci_slot: Option<&str>) -> Option<String> {
    if !proc_root.exists() {
        return None;
    }
    if let Some(slot) = pci_slot
        && let Ok(text) = std::fs::read_to_string(proc_root.join(slot).join("information"))
        && let Some(model) = parse_nvidia_gpu_info(&text)
    {
        return Some(model);
    }
    let entries = std::fs::read_dir(proc_root).ok()?;
    let mut dirs: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        if let Ok(text) = std::fs::read_to_string(dir.join("information"))
            && let Some(model) = parse_nvidia_gpu_info(&text)
        {
            return Some(model);
        }
    }
    None
}
