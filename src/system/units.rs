//! Typed capacity units shared across memory, VRAM/GTT, disk storage, and network counters.

use std::fmt;

/// Typed memory/storage capacity unit preventing ambiguity between bytes and KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ByteSize(u64);

impl ByteSize {
    pub const ZERO: Self = Self(0);

    #[inline]
    #[must_use]
    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    #[inline]
    #[must_use]
    pub const fn from_kib(kib: u64) -> Self {
        Self(kib.saturating_mul(1024))
    }

    #[inline]
    #[must_use]
    pub const fn from_mib(mib: u64) -> Self {
        Self(mib.saturating_mul(1024 * 1024))
    }

    #[inline]
    #[must_use]
    pub const fn as_bytes(self) -> u64 {
        self.0
    }

    #[inline]
    #[must_use]
    pub fn as_kib(self) -> f64 {
        self.0 as f64 / 1024.0
    }

    #[inline]
    #[must_use]
    pub fn as_mib(self) -> f64 {
        self.0 as f64 / (1024.0 * 1024.0)
    }

    #[inline]
    #[must_use]
    pub fn as_gib(self) -> f64 {
        self.0 as f64 / (1024.0 * 1024.0 * 1024.0)
    }

    #[inline]
    #[must_use]
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 >= 1024 * 1024 * 1024 {
            write!(f, "{:.2} GiB", self.as_gib())
        } else if self.0 >= 1024 * 1024 {
            write!(f, "{:.1} MiB", self.as_mib())
        } else if self.0 >= 1024 {
            write!(f, "{:.0} KiB", self.as_kib())
        } else {
            write!(f, "{} B", self.0)
        }
    }
}

/// Frequency display/conversion unit for CPU clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FreqUnit {
    Khz,
    Mhz,
    #[default]
    Ghz,
}

impl FreqUnit {
    /// Converts a frequency in kHz into the target unit.
    #[inline]
    #[must_use]
    pub fn convert_khz(self, khz: u64) -> f64 {
        match self {
            Self::Khz => khz as f64,
            Self::Mhz => khz as f64 / 1_000.0,
            Self::Ghz => khz as f64 / 1_000_000.0,
        }
    }

    /// Formats a kHz value with unit suffix (`kHz`, `MHz`, or `GHz`).
    #[must_use]
    pub fn format_khz(self, khz: u64) -> String {
        match self {
            Self::Khz => format!("{khz} kHz"),
            Self::Mhz => format!("{:.0} MHz", self.convert_khz(khz)),
            Self::Ghz => format!("{:.2} GHz", self.convert_khz(khz)),
        }
    }
}

/// Target unit or auto-scaling mode for network throughput formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RateUnit {
    #[default]
    AutoBytes,
    AutoBits,
    BytesPerSec,
    KilobytesPerSec,
    MegabytesPerSec,
    GigabytesPerSec,
    BitsPerSec,
    KilobitsPerSec,
    MegabitsPerSec,
    GigabitsPerSec,
}

/// Typed data transfer rate stored in bytes per second.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct DataRate(f64);

impl DataRate {
    pub const ZERO: Self = Self(0.0);

    #[inline]
    #[must_use]
    pub const fn from_bytes_per_sec(bytes_per_sec: f64) -> Self {
        Self(if bytes_per_sec.is_finite() && bytes_per_sec > 0.0 {
            bytes_per_sec
        } else {
            0.0
        })
    }

    #[inline]
    #[must_use]
    pub const fn as_bytes_per_sec(self) -> f64 {
        self.0
    }

    #[inline]
    #[must_use]
    pub fn as_bits_per_sec(self) -> f64 {
        self.0 * 8.0
    }

    /// Returns the numeric value converted to the requested [`RateUnit`].
    #[must_use]
    pub fn value_in(self, unit: RateUnit) -> f64 {
        match unit {
            RateUnit::AutoBytes | RateUnit::BytesPerSec => self.0,
            RateUnit::KilobytesPerSec => self.0 / 1_000.0,
            RateUnit::MegabytesPerSec => self.0 / 1_000_000.0,
            RateUnit::GigabytesPerSec => self.0 / 1_000_000_000.0,
            RateUnit::AutoBits | RateUnit::BitsPerSec => self.as_bits_per_sec(),
            RateUnit::KilobitsPerSec => self.as_bits_per_sec() / 1_000.0,
            RateUnit::MegabitsPerSec => self.as_bits_per_sec() / 1_000_000.0,
            RateUnit::GigabitsPerSec => self.as_bits_per_sec() / 1_000_000_000.0,
        }
    }

    /// Formats the throughput rate according to `unit` (auto-scaled or locked).
    #[must_use]
    pub fn format(self, unit: RateUnit) -> String {
        match unit {
            RateUnit::AutoBytes => format_si_scaled(self.0, &["B/s", "KB/s", "MB/s", "GB/s"]),
            RateUnit::AutoBits => {
                format_si_scaled(self.as_bits_per_sec(), &["b/s", "Kb/s", "Mb/s", "Gb/s"])
            }
            RateUnit::BytesPerSec => format!("{:.0} B/s", self.0),
            RateUnit::KilobytesPerSec => format!("{:.1} KB/s", self.value_in(unit)),
            RateUnit::MegabytesPerSec => format!("{:.2} MB/s", self.value_in(unit)),
            RateUnit::GigabytesPerSec => format!("{:.2} GB/s", self.value_in(unit)),
            RateUnit::BitsPerSec => format!("{:.0} b/s", self.as_bits_per_sec()),
            RateUnit::KilobitsPerSec => format!("{:.1} Kb/s", self.value_in(unit)),
            RateUnit::MegabitsPerSec => format!("{:.2} Mb/s", self.value_in(unit)),
            RateUnit::GigabitsPerSec => format!("{:.2} Gb/s", self.value_in(unit)),
        }
    }
}

fn format_si_scaled(val: f64, units: &[&str; 4]) -> String {
    let mut scaled = val.max(0.0);
    let mut idx = 0;
    while scaled >= 1_000.0 && idx + 1 < units.len() {
        scaled /= 1_000.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{scaled:.1}{}", units[idx])
    } else if scaled >= 100.0 {
        format!("{scaled:.0}{}", units[idx])
    } else {
        format!("{scaled:.1}{}", units[idx])
    }
}

impl fmt::Display for DataRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format(RateUnit::AutoBytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freq_unit_converts_and_formats() {
        assert!((FreqUnit::Mhz.convert_khz(2_500_000) - 2500.0).abs() < 1e-6);
        assert!((FreqUnit::Ghz.convert_khz(2_500_000) - 2.5).abs() < 1e-6);
        assert_eq!(FreqUnit::Mhz.format_khz(2_500_000), "2500 MHz");
        assert_eq!(FreqUnit::Ghz.format_khz(2_500_000), "2.50 GHz");
    }

    #[test]
    fn data_rate_formats_auto_and_locked_units() {
        let rate = DataRate::from_bytes_per_sec(1_500_000.0);
        assert_eq!(rate.format(RateUnit::AutoBytes), "1.5MB/s");
        assert_eq!(rate.format(RateUnit::AutoBits), "12.0Mb/s");
        assert_eq!(rate.format(RateUnit::MegabitsPerSec), "12.00 Mb/s");
        assert_eq!(rate.format(RateUnit::KilobytesPerSec), "1500.0 KB/s");
    }
}
