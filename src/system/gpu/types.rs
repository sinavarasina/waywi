//! Normalized GPU telemetry snapshot types across AMD, Intel, and NVIDIA hardware.

use std::time::Instant;

use crate::system::units::ByteSize;

/// Normalized PCI vendor identification for Linux DRM GPUs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuVendor {
    Amd,
    Intel,
    Nvidia,
    Other(u16),
}

impl GpuVendor {
    #[must_use]
    pub const fn from_pci_id(id: u16) -> Self {
        match id {
            0x1002 => Self::Amd,
            0x8086 => Self::Intel,
            0x10de => Self::Nvidia,
            other => Self::Other(other),
        }
    }
}

/// Telemetry for one discovered Linux DRM GPU (`/sys/class/drm/card*`).
///
/// Fields that a vendor/driver/permission level does not expose remain `None` rather than
/// falling back to synthetic estimates or external subprocesses.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuDeviceSnapshot {
    pub card_name: String,
    pub vendor: GpuVendor,
    pub pci_vendor_id: Option<u16>,
    pub pci_device_id: Option<u16>,
    pub driver: Option<String>,
    pub model_name: Option<String>,
    pub utilization_percent: Option<f32>,
    pub vram_used: Option<ByteSize>,
    pub vram_total: Option<ByteSize>,
    pub gtt_used: Option<ByteSize>,
    pub gtt_total: Option<ByteSize>,
    pub core_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    pub temperature_celsius: Option<f32>,
    pub hotspot_celsius: Option<f32>,
    pub power_watts: Option<f32>,
    pub fan_rpm: Option<u32>,
}

/// Multi-GPU telemetry snapshot returned by [`super::read_gpus`] and `System::gpu()`.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuSnapshot {
    pub gpus: Vec<GpuDeviceSnapshot>,
    pub timestamp: Instant,
}

impl GpuSnapshot {
    /// Returns the primary GPU (preferring cards exposing utilization metrics, else the first card).
    #[must_use]
    pub fn primary(&self) -> Option<&GpuDeviceSnapshot> {
        self.gpus
            .iter()
            .find(|g| g.utilization_percent.is_some())
            .or_else(|| self.gpus.first())
    }
}
