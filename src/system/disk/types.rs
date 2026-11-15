//! Storage mount capacity and block I/O throughput snapshot types.

use std::path::PathBuf;
use std::time::Instant;

use crate::system::units::ByteSize;

/// Capacity and usage snapshot for one mounted local filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskMount {
    pub device: String,
    pub mount_point: PathBuf,
    pub fs_type: String,
    pub total: ByteSize,
    pub available: ByteSize,
    pub free: ByteSize,
    pub used: ByteSize,
}

impl DiskMount {
    /// Percentage of total space currently used (`0.0..=100.0`).
    #[must_use]
    pub fn usage_percent(&self) -> f32 {
        let total = self.total.as_bytes();
        if total == 0 {
            return 0.0;
        }
        ((self.used.as_bytes() as f64 / total as f64) * 100.0) as f32
    }
}

/// Cumulative counters and computed throughput/IOPS for one block device.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskDeviceIo {
    pub name: String,
    pub is_whole_disk: bool,
    pub reads_completed: u64,
    pub writes_completed: u64,
    pub read_bytes: ByteSize,
    pub write_bytes: ByteSize,
    pub read_bytes_per_sec: f64,
    pub write_bytes_per_sec: f64,
    pub read_iops: f64,
    pub write_iops: f64,
}

/// Combined filesystem capacity + block I/O snapshot emitted by `System::disk()`.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskSnapshot {
    pub mounts: Vec<DiskMount>,
    pub io: Vec<DiskDeviceIo>,
    /// Summed read throughput across whole physical/virtual disks (`is_whole_disk`).
    pub total_read_bytes_per_sec: f64,
    /// Summed write throughput across whole physical/virtual disks (`is_whole_disk`).
    pub total_write_bytes_per_sec: f64,
    pub timestamp: Instant,
}
