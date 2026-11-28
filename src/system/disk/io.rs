//! `/proc/diskstats` parser and stateful delta I/O sampler.

use std::collections::HashMap;
use std::time::Instant;

use crate::error::Result;
use crate::system::units::{ByteSize, DataRate};

use super::mounts::read_disk_mounts;
use super::types::{DiskDeviceIo, DiskMount, DiskSnapshot};

const SECTOR_BYTES: u64 = 512;

/// Cumulative per-device counters parsed from `/proc/diskstats`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskIoCounters {
    pub name: String,
    pub is_whole_disk: bool,
    pub reads_completed: u64,
    pub sectors_read: u64,
    pub writes_completed: u64,
    pub sectors_written: u64,
}

/// Parses `/proc/diskstats`, excluding `loop*` and `ram*` pseudo block devices.
#[must_use]
pub fn parse_proc_diskstats(contents: &str) -> Vec<DiskIoCounters> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 {
            continue;
        }
        let name = cols[2];
        if name.starts_with("loop") || name.starts_with("ram") {
            continue;
        }
        let Ok(reads_completed) = cols[3].parse::<u64>() else {
            continue;
        };
        let Ok(sectors_read) = cols[5].parse::<u64>() else {
            continue;
        };
        let Ok(writes_completed) = cols[7].parse::<u64>() else {
            continue;
        };
        let Ok(sectors_written) = cols[9].parse::<u64>() else {
            continue;
        };
        out.push(DiskIoCounters {
            name: name.to_owned(),
            is_whole_disk: is_whole_block_device(name),
            reads_completed,
            sectors_read,
            writes_completed,
            sectors_written,
        });
    }
    out
}

fn is_whole_block_device(name: &str) -> bool {
    // Stacked device-mapper (LUKS/LVM) and RAM-backed zram devices sit above physical block
    // devices or memory; excluding them from `is_whole_disk` prevents double-counting totals.
    if name.starts_with("dm-") || name.starts_with("zram") {
        return false;
    }
    if let Some(rest) = name.strip_prefix("nvme") {
        // `nvme<ctrl>n<ns>` is whole namespace; `nvme<ctrl>n<ns>p<part>` is a partition.
        let Some((ctrl, ns)) = rest.split_once('n') else {
            return false;
        };
        return !ctrl.is_empty()
            && !ns.is_empty()
            && ctrl.bytes().all(|b| b.is_ascii_digit())
            && ns.bytes().all(|b| b.is_ascii_digit());
    }
    if let Some(rest) = name.strip_prefix("mmcblk") {
        // `mmcblk<n>` is whole card; `mmcblk<n>p<k>`, `mmcblk<n>boot<k>`, `mmcblk<n>rpmb` are not.
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    }
    if let Some(rest) = name.strip_prefix("md").or_else(|| name.strip_prefix("nbd")) {
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    }
    if let Some(rest) = name
        .strip_prefix("xvd")
        .or_else(|| name.strip_prefix("sd"))
        .or_else(|| name.strip_prefix("vd"))
        .or_else(|| name.strip_prefix("hd"))
    {
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphabetic());
    }
    true
}

/// Stateful sampler computing block read/write throughput (B/s) and IOPS from `/proc/diskstats`.
#[derive(Debug, Default)]
pub struct DiskSampler {
    prev: HashMap<String, DiskIoCounters>,
    prev_time: Option<Instant>,
}

impl DiskSampler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.prev.clear();
        self.prev.shrink_to_fit();
        self.prev_time = None;
    }

    /// Reads current mounts (`statvfs`) + `/proc/diskstats` delta rates.
    pub fn sample(&mut self) -> Result<DiskSnapshot> {
        let mounts = read_disk_mounts().unwrap_or_default();
        let diskstats = std::fs::read_to_string("/proc/diskstats")?;
        Ok(self.feed_snapshot(mounts, &diskstats, Instant::now()))
    }

    /// Pure deterministic step computing I/O throughput deltas from provided `/proc/diskstats`.
    pub fn feed_snapshot(
        &mut self,
        mounts: Vec<DiskMount>,
        diskstats: &str,
        now: Instant,
    ) -> DiskSnapshot {
        let counters = parse_proc_diskstats(diskstats);
        let dt_secs = self
            .prev_time
            .map(|t| now.saturating_duration_since(t).as_secs_f64())
            .filter(|dt| *dt > 0.0);

        let mut io = Vec::with_capacity(counters.len());
        let mut total_read_bytes_per_sec = 0.0;
        let mut total_write_bytes_per_sec = 0.0;
        let mut next_prev = HashMap::with_capacity(counters.len());

        for cur in counters {
            let (read_bps, write_bps, read_iops, write_iops) =
                match (dt_secs, self.prev.get(&cur.name)) {
                    (Some(dt), Some(prev)) => {
                        let dr_bytes = cur
                            .sectors_read
                            .saturating_sub(prev.sectors_read)
                            .saturating_mul(SECTOR_BYTES);
                        let dw_bytes = cur
                            .sectors_written
                            .saturating_sub(prev.sectors_written)
                            .saturating_mul(SECTOR_BYTES);
                        let dr_ops = cur.reads_completed.saturating_sub(prev.reads_completed);
                        let dw_ops = cur.writes_completed.saturating_sub(prev.writes_completed);
                        (
                            dr_bytes as f64 / dt,
                            dw_bytes as f64 / dt,
                            dr_ops as f64 / dt,
                            dw_ops as f64 / dt,
                        )
                    }
                    _ => (0.0, 0.0, 0.0, 0.0),
                };

            if cur.is_whole_disk {
                total_read_bytes_per_sec += read_bps;
                total_write_bytes_per_sec += write_bps;
            }

            io.push(DiskDeviceIo {
                name: cur.name.clone(),
                is_whole_disk: cur.is_whole_disk,
                reads_completed: cur.reads_completed,
                writes_completed: cur.writes_completed,
                read_bytes: ByteSize::from_bytes(cur.sectors_read.saturating_mul(SECTOR_BYTES)),
                write_bytes: ByteSize::from_bytes(cur.sectors_written.saturating_mul(SECTOR_BYTES)),
                read_rate: DataRate::from_bytes_per_sec(read_bps),
                write_rate: DataRate::from_bytes_per_sec(write_bps),
                read_iops,
                write_iops,
            });
            next_prev.insert(cur.name.clone(), cur);
        }

        self.prev = next_prev;
        self.prev_time = Some(now);

        DiskSnapshot {
            mounts,
            io,
            total_read_rate: DataRate::from_bytes_per_sec(total_read_bytes_per_sec),
            total_write_rate: DataRate::from_bytes_per_sec(total_write_bytes_per_sec),
            timestamp: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn disk_sampler_computes_typed_data_rates_without_double_counting_partitions() {
        let s0 = "\
   7       0 loop0 0 0 0 0 0 0 0 0 0 0 0
 259       0 nvme0n1 100 0 2000 10 50 0 1000 5 0 0 0
 259       1 nvme0n1p1 100 0 2000 10 50 0 1000 5 0 0 0
";
        let s1 = "\
   7       0 loop0 0 0 0 0 0 0 0 0 0 0 0
 259       0 nvme0n1 300 0 6000 20 150 0 5000 15 0 0 0
 259       1 nvme0n1p1 300 0 6000 20 150 0 5000 15 0 0 0
";
        let mut sampler = DiskSampler::new();
        let t0 = Instant::now();
        let snap0 = sampler.feed_snapshot(Vec::new(), s0, t0);
        assert_eq!(snap0.total_read_rate, DataRate::ZERO);
        assert_eq!(snap0.total_write_rate, DataRate::ZERO);
        assert_eq!(snap0.io[0].read_rate, DataRate::ZERO);
        assert_eq!(snap0.io[0].write_rate, DataRate::ZERO);

        let snap1 = sampler.feed_snapshot(Vec::new(), s1, t0 + Duration::from_secs(2));
        assert!((snap1.total_read_rate.as_bytes_per_sec() - 1_024_000.0).abs() < 1.0);
        assert!((snap1.total_write_rate.as_bytes_per_sec() - 1_024_000.0).abs() < 1.0);
        assert!((snap1.io[0].read_rate.as_bytes_per_sec() - 1_024_000.0).abs() < 1.0);
        assert!((snap1.io[0].write_rate.as_bytes_per_sec() - 1_024_000.0).abs() < 1.0);
    }

    #[test]
    fn whole_block_device_excludes_partitions_dm_and_zram() {
        assert!(is_whole_block_device("nvme0n1"));
        assert!(!is_whole_block_device("nvme0n1p1"));
        assert!(is_whole_block_device("sda"));
        assert!(!is_whole_block_device("sda1"));
        assert!(is_whole_block_device("xvda"));
        assert!(!is_whole_block_device("xvda2"));
        assert!(is_whole_block_device("mmcblk0"));
        assert!(!is_whole_block_device("mmcblk0p1"));
        assert!(!is_whole_block_device("mmcblk0boot0"));
        assert!(is_whole_block_device("md0"));
        assert!(!is_whole_block_device("md0p1"));
        assert!(!is_whole_block_device("dm-0"));
        assert!(!is_whole_block_device("zram0"));
    }
}
