//! Storage mount capacity (`statvfs`) and block I/O telemetry domain.

pub mod io;
pub mod mounts;
pub mod types;

pub use io::{DiskIoCounters, DiskSampler, parse_proc_diskstats};
pub use mounts::{ProcMountEntry, parse_proc_mounts, read_disk_mounts};
pub use types::{DiskDeviceIo, DiskMount, DiskSnapshot};
