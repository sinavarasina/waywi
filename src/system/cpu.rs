use std::time::Instant;

use crate::error::Result;
use crate::system::os::{LoadAverage, read_load_average};
use crate::system::sysfs::read_sysfs_u64;
use crate::system::units::FreqUnit;

/// Raw cumulative CPU jiffies parsed from one `/proc/stat` `cpu*` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuCounters {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuCounters {
    #[must_use]
    pub const fn idle_total(self) -> u64 {
        self.idle + self.iowait
    }

    #[must_use]
    pub const fn total(self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    /// Computes utilization percentage (`0.0..=100.0`) from `prev` to `self`.
    #[must_use]
    pub fn utilization_since(self, prev: Self) -> f32 {
        let total_delta = self.total().saturating_sub(prev.total());
        if total_delta == 0 {
            return 0.0;
        }
        let idle_delta = self.idle_total().saturating_sub(prev.idle_total());
        let active = total_delta.saturating_sub(idle_delta);
        ((active as f64 / total_delta as f64) * 100.0).clamp(0.0, 100.0) as f32
    }
}

/// CPU utilization, per-core frequency, and load-average snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct CpuSnapshot {
    /// Aggregate utilization across all logical cores (`0.0..=100.0`).
    pub total_percent: f32,
    /// Per-logical-core utilization (`0.0..=100.0`).
    pub per_core_percent: Vec<f32>,
    /// Per-logical-core current frequency in kHz (from cpufreq sysfs).
    pub per_core_freq_khz: Vec<u64>,
    /// System load averages (`1m`, `5m`, `15m`).
    pub load_avg: LoadAverage,
    /// Number of logical CPU cores discovered.
    pub logical_cores: usize,
    /// Monotonic sample timestamp.
    pub timestamp: Instant,
}

impl CpuSnapshot {
    /// Average frequency across all reported cores in kHz.
    #[must_use]
    pub fn avg_freq_khz(&self) -> Option<u64> {
        if self.per_core_freq_khz.is_empty() {
            return None;
        }
        let sum: u64 = self.per_core_freq_khz.iter().copied().sum();
        Some(sum / self.per_core_freq_khz.len() as u64)
    }

    /// Average frequency converted to the requested [`FreqUnit`].
    #[must_use]
    pub fn avg_freq(&self, unit: FreqUnit) -> Option<f64> {
        self.avg_freq_khz().map(|khz| unit.convert_khz(khz))
    }

    /// Minimum core frequency converted to the requested [`FreqUnit`].
    #[must_use]
    pub fn min_freq(&self, unit: FreqUnit) -> Option<f64> {
        self.per_core_freq_khz
            .iter()
            .copied()
            .min()
            .map(|khz| unit.convert_khz(khz))
    }

    /// Maximum core frequency converted to the requested [`FreqUnit`].
    #[must_use]
    pub fn max_freq(&self, unit: FreqUnit) -> Option<f64> {
        self.per_core_freq_khz
            .iter()
            .copied()
            .max()
            .map(|khz| unit.convert_khz(khz))
    }
}

/// Shared CPU sampler holding the previous `/proc/stat` counters.
pub struct CpuSampler {
    prev_total: Option<CpuCounters>,
    prev_cores: Vec<CpuCounters>,
    sample_reads: u64,
}

impl Default for CpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuSampler {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_total: None,
            prev_cores: Vec::new(),
            sample_reads: 0,
        }
    }

    pub fn reset(&mut self) {
        self.prev_total = None;
        self.prev_cores.clear();
        self.prev_cores.shrink_to_fit();
    }

    #[must_use]
    pub const fn sample_reads(&self) -> u64 {
        self.sample_reads
    }

    /// Reads `/proc/stat`, `/proc/loadavg`, and `cpufreq` sysfs once, always returning a
    /// [`CpuSnapshot`] (with `0.0` utilization on the initial baseline read or if `/proc/stat`
    /// cannot be read).
    pub fn sample_snapshot(&mut self) -> CpuSnapshot {
        let now = Instant::now();
        let Ok(contents) = std::fs::read_to_string("/proc/stat") else {
            return CpuSnapshot {
                total_percent: 0.0,
                per_core_percent: Vec::new(),
                per_core_freq_khz: Vec::new(),
                load_avg: read_load_average().unwrap_or_default(),
                logical_cores: 0,
                timestamp: now,
            };
        };
        let mut snap = match self.feed_proc_stat(&contents, now) {
            Some(snap) => snap,
            None => {
                let logical_cores = self.prev_cores.len();
                CpuSnapshot {
                    total_percent: 0.0,
                    per_core_percent: vec![0.0; logical_cores],
                    per_core_freq_khz: Vec::new(),
                    load_avg: LoadAverage::default(),
                    logical_cores,
                    timestamp: now,
                }
            }
        };
        snap.per_core_freq_khz = read_per_core_freq_khz(snap.logical_cores);
        snap.load_avg = read_load_average().unwrap_or_default();
        snap
    }

    /// Reads `/proc/stat`, `/proc/loadavg`, and `cpufreq` sysfs once, returning `Some(CpuSnapshot)`
    /// once two `/proc/stat` samples exist.
    pub fn sample_proc_stat(&mut self) -> Result<Option<CpuSnapshot>> {
        let contents = std::fs::read_to_string("/proc/stat")?;
        let Some(mut snap) = self.feed_proc_stat(&contents, Instant::now()) else {
            return Ok(None);
        };
        snap.per_core_freq_khz = read_per_core_freq_khz(snap.logical_cores);
        snap.load_avg = read_load_average().unwrap_or_default();
        Ok(Some(snap))
    }

    /// Pure parser/delta calculator accepting arbitrary `/proc/stat` text (unit-testable).
    pub fn feed_proc_stat(&mut self, contents: &str, now: Instant) -> Option<CpuSnapshot> {
        self.sample_reads += 1;
        let (total, cores) = parse_proc_stat(contents)?;

        let result = if let Some(prev_total) = self.prev_total {
            let total_percent = total.utilization_since(prev_total);
            let per_core_percent = cores
                .iter()
                .enumerate()
                .map(|(i, cur)| {
                    self.prev_cores
                        .get(i)
                        .map_or(0.0, |prev| cur.utilization_since(*prev))
                })
                .collect();
            Some(CpuSnapshot {
                total_percent,
                per_core_percent,
                per_core_freq_khz: Vec::new(),
                load_avg: LoadAverage::default(),
                logical_cores: cores.len(),
                timestamp: now,
            })
        } else {
            None
        };

        self.prev_total = Some(total);
        self.prev_cores = cores;
        result
    }
}

/// Parses aggregate `cpu` and per-core `cpuN` counters from `/proc/stat`.
#[must_use]
pub fn parse_proc_stat(contents: &str) -> Option<(CpuCounters, Vec<CpuCounters>)> {
    let mut total = None;
    let mut cores = Vec::new();

    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else {
            continue;
        };
        if label == "cpu" {
            total = Some(parse_counter_fields(parts));
        } else if let Some(suffix) = label.strip_prefix("cpu")
            && !suffix.is_empty()
            && suffix.bytes().all(|b| b.is_ascii_digit())
        {
            cores.push(parse_counter_fields(parts));
        }
    }

    total.map(|t| (t, cores))
}

fn parse_counter_fields<'a>(mut parts: impl Iterator<Item = &'a str>) -> CpuCounters {
    let mut next_u64 = || -> u64 { parts.next().and_then(|s| s.parse().ok()).unwrap_or(0) };
    CpuCounters {
        user: next_u64(),
        nice: next_u64(),
        system: next_u64(),
        idle: next_u64(),
        iowait: next_u64(),
        irq: next_u64(),
        softirq: next_u64(),
        steal: next_u64(),
    }
}

fn read_per_core_freq_khz(logical_cores: usize) -> Vec<u64> {
    let mut freqs = Vec::with_capacity(logical_cores);
    for core in 0..logical_cores {
        let scaling = format!("/sys/devices/system/cpu/cpu{core}/cpufreq/scaling_cur_freq");
        let khz = read_sysfs_u64(std::path::Path::new(&scaling)).or_else(|| {
            let info = format!("/sys/devices/system/cpu/cpu{core}/cpufreq/cpuinfo_cur_freq");
            read_sysfs_u64(std::path::Path::new(&info))
        });
        if let Some(v) = khz {
            freqs.push(v);
        }
    }
    freqs
}
